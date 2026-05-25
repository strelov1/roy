use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use teloxide::Bot;
use tracing_subscriber::EnvFilter;

use roy_gateway::binder::SessionBinder;
use roy_gateway::cancel::CancelRegistry;
use roy_gateway::config::GatewayConfig;
use roy_gateway::daemon::RealConnFactory;
use roy_gateway::orchestrator::OrchestratorConfig;
use roy_gateway::telegram::{run, BotDeps, TeloxideReplier};
use roy_gateway::ws;

#[derive(Parser, Debug)]
#[command(name = "roy-gateway")]
struct Args {
    /// Path to the gateway TOML config.
    #[arg(long)]
    config: PathBuf,
}

fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

fn default_ws_token_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-gateway/ws.token")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("roy_gateway=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let cfg = GatewayConfig::load(&args.config)
        .with_context(|| format!("loading {}", args.config.display()))?;

    let socket_path = cfg
        .daemon
        .socket
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(default_socket);
    tracing::info!(socket = %socket_path.display(), "daemon socket");

    let telegram_task = build_telegram_task(&cfg, &socket_path).await?;
    let ws_task = build_ws_task(&cfg, &socket_path)?;

    // At least one is Some (validated in GatewayConfig::load).
    match (telegram_task, ws_task) {
        (Some(tg), Some(ws)) => {
            tokio::select! {
                r = tg => r.context("telegram task")?,
                r = ws => r.context("ws task")?,
            }
        }
        (Some(tg), None) => tg.await.context("telegram task")?,
        (None, Some(ws)) => ws.await.context("ws task")?,
        (None, None) => unreachable!("validate() guarantees at least one adapter"),
    }
}

async fn build_telegram_task(
    cfg: &GatewayConfig,
    socket_path: &Path,
) -> Result<Option<tokio::task::JoinHandle<Result<()>>>> {
    let Some(tg) = &cfg.telegram else {
        return Ok(None);
    };
    let binder_cfg = cfg
        .binder
        .as_ref()
        .expect("validate() guarantees binder when telegram is set");
    let binder_path = PathBuf::from(&binder_cfg.path);
    let binder = Arc::new(
        SessionBinder::load(binder_path.clone())
            .await
            .with_context(|| format!("loading binder {}", binder_path.display()))?,
    );
    let conn_factory = Arc::new(RealConnFactory::new(socket_path.to_path_buf()));
    let orch_cfg = Arc::new(OrchestratorConfig {
        preset: tg.preset.clone(),
        project_id: tg.project_id.clone(),
        turn_timeout: Duration::from_secs(tg.turn_timeout_secs),
        typing_interval: Duration::from_secs(4),
    });
    let bot = Bot::new(tg.token.clone());
    let replier = Arc::new(TeloxideReplier::new(bot.clone()));
    let allowed: HashSet<u64> = tg.allowed_user_ids.iter().copied().collect();
    let deps = BotDeps {
        cfg: orch_cfg,
        binder,
        conn_factory,
        replier,
        cancel_registry: CancelRegistry::new(),
        allowed_user_ids: Arc::new(allowed),
    };
    Ok(Some(tokio::spawn(async move { run(bot, deps).await })))
}

fn build_ws_task(
    cfg: &GatewayConfig,
    socket_path: &Path,
) -> Result<Option<tokio::task::JoinHandle<Result<()>>>> {
    let Some(ws_cfg) = &cfg.websocket else {
        return Ok(None);
    };
    let addr: std::net::SocketAddr = ws_cfg
        .bind
        .parse()
        .with_context(|| format!("parsing websocket.bind '{}'", ws_cfg.bind))?;
    let token_path = ws_cfg
        .token_path
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(default_ws_token_path);
    let token = Arc::new(ws::load_or_create_ws_token(&token_path)?);
    tracing::info!(path = %token_path.display(), %addr, "ws auth token / bind");
    let socket: Arc<Path> = Arc::from(socket_path.to_path_buf());
    Ok(Some(tokio::spawn(async move {
        ws::run_ws_relay(addr, token, socket).await
    })))
}
