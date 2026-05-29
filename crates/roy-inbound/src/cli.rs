//! `roy-inbound` entry point. Loads config, opens DB, spawns publishers
//! and the dispatcher, awaits ctrl-c.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use crate::bus::{self, EventRef};
use crate::channels::telegram::reply::{NoopSender, TelegramReplyHook};
use crate::channels::telegram::{ManagementClient, TelegramPublisher, TelegramRegistry};
use crate::channels::webhook::{WebhookPublisher, WebhookSourceSpec};
use crate::channels::Publisher;
use crate::config::InboundConfig;
use crate::dispatcher::InboundDispatcher;
use crate::reply::{ReplyHook, ReplyHookRegistry};
use crate::router::{CompositeRouter, ConfigRouter, TelegramRouter};
use crate::session::SessionResolver;
use crate::store::{bindings::BindingStore, db};

#[derive(clap::Parser, Debug)]
#[command(name = "roy-inbound", about = "Inbound event bus for roy")]
pub struct Args {
    /// Path to the inbound TOML config.
    #[arg(long)]
    pub config: PathBuf,
    /// SQLite DB path (default ~/.local/state/roy-inbound/state.db).
    #[arg(long, env = "ROY_INBOUND_DB")]
    pub db: Option<PathBuf>,
    /// roy daemon Unix socket.
    #[arg(long, env = "ROY_SOCKET")]
    pub socket: Option<PathBuf>,
    /// Default harness used when resolving Spawn targets.
    #[arg(long, default_value = "claude")]
    pub harness: String,
    /// roy-management base URL for resolving telegram bot→agent bindings.
    #[arg(
        long,
        env = "ROY_MANAGEMENT_URL",
        default_value = "http://127.0.0.1:8079"
    )]
    pub management_url: String,
}

pub async fn run(args: Args) -> Result<()> {
    let cfg = InboundConfig::load(&args.config)
        .with_context(|| format!("loading {}", args.config.display()))?;

    let db_path = args.db.unwrap_or_else(default_db_path);
    let pool = db::open(&db_path).await?;
    let bindings = Arc::new(BindingStore::new(pool));

    let socket_path = args
        .socket
        .unwrap_or_else(roy_protocol::wire::default_socket_path);

    let (bus_tx, bus_rx) = bus::channel(cfg.bus.capacity);

    // Registry for Telegram bots.
    let tg_registry = TelegramRegistry::new();

    // Reply-hook registry: register both webhook and telegram.
    let mut hooks = ReplyHookRegistry::new();
    hooks.register(
        "webhook",
        Box::new(|ev: &EventRef| -> Box<dyn ReplyHook> {
            Box::new(crate::channels::webhook::reply::WebhookReplyHook::new(
                ev.id.to_string(),
            ))
        }),
    );
    {
        let reg = tg_registry.clone();
        hooks.register(
            "telegram",
            Box::new(move |ev: &EventRef| -> Box<dyn ReplyHook> {
                let chat_id = ev.sender_id.parse::<i64>().unwrap_or(0);
                match reg.sender_for(&ev.source_id) {
                    Some(sender) => Box::new(TelegramReplyHook::new(sender, chat_id)),
                    None => Box::new(TelegramReplyHook::new(Arc::new(NoopSender), chat_id)),
                }
            }),
        );
    }
    let hooks = Arc::new(hooks);

    // Build the webhook publisher from config (one source per webhook).
    let webhook_sources: Vec<_> = cfg
        .sources
        .iter()
        .filter(|s| s.kind == "webhook")
        .map(|s| WebhookSourceSpec {
            source_id: s.id.clone(),
            config: s.webhook.clone().expect("validated in InboundConfig::load"),
        })
        .collect();
    let bind: std::net::SocketAddr = cfg
        .server
        .bind
        .parse()
        .with_context(|| format!("parsing server.bind '{}'", cfg.server.bind))?;
    let webhook = Arc::new(WebhookPublisher::new(bind, webhook_sources)?);

    // Build the Telegram publisher (only when ROY_INTERNAL_TOKEN is set).
    let mgmt_token = std::env::var("ROY_INTERNAL_TOKEN").ok();
    let tg_publisher = mgmt_token.as_ref().map(|tok| {
        Arc::new(TelegramPublisher::new(
            tg_registry.clone(),
            Arc::new(ManagementClient::new(
                args.management_url.clone(),
                tok.clone(),
            )),
        ))
    });
    if tg_publisher.is_none() {
        tracing::info!("ROY_INTERNAL_TOKEN not set; Telegram publisher disabled");
    }

    let router: Arc<dyn crate::router::Router> = Arc::new(CompositeRouter {
        telegram: TelegramRouter::new(tg_registry.clone()),
        config: ConfigRouter::from_config(&cfg),
    });
    let resolver = SessionResolver::new(bindings.clone(), args.harness);

    let dispatcher = InboundDispatcher {
        bus: bus_rx,
        router,
        resolver,
        bindings: bindings.clone(),
        hooks: hooks.clone(),
        socket_path,
    };

    let cancel = CancellationToken::new();
    let cancel_pub = cancel.clone();
    let cancel_disp = cancel.clone();

    let dispatcher_handle = tokio::spawn(async move {
        if let Err(e) = dispatcher.run(cancel_disp).await {
            tracing::error!(error = ?e, "dispatcher exited with error");
        }
    });

    // Clone bus_tx before moving it into the webhook task.
    let bus_tx_tg = bus_tx.clone();

    let pub_handle = tokio::spawn(async move {
        if let Err(e) = webhook.run(bus_tx, cancel_pub).await {
            tracing::error!(error = ?e, "webhook publisher exited with error");
        }
    });

    let tg_handle = tg_publisher.map(|pubr| {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = pubr.run(bus_tx_tg, cancel).await {
                tracing::error!(error = ?e, "telegram publisher exited with error");
            }
        })
    });

    tokio::signal::ctrl_c()
        .await
        .context("waiting for ctrl-c")?;
    tracing::info!("ctrl-c received; shutting down");
    cancel.cancel();
    let tg_join = async {
        if let Some(h) = tg_handle {
            let _ = h.await;
        }
    };
    let _ = tokio::join!(dispatcher_handle, pub_handle, tg_join);
    Ok(())
}

fn default_db_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-inbound/state.db")
}
