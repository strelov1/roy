//! Roy → chat-platform gateway. v1 supports a single channel: Telegram.
//!
//! Architecture: one long-lived process per gateway, talks to a running
//! `roy serve` daemon over its Unix socket. Each turn opens a `TurnConn`
//! that drives Spawn/Resume → AcquireInput → Send → Frame stream → ReleaseInput.
//! `(chat_id → roy session_id)` is persisted in a JSON file so chats
//! survive restarts.
//!
//! See `docs/superpowers/plans/2026-05-23-roy-gateway-telegram.md`.

pub mod binder;
pub mod cancel;
pub mod config;
pub mod daemon;
pub mod draft_stream;
pub mod formatting;
pub mod orchestrator;
pub mod telegram;
pub mod typing;
pub mod ws;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use teloxide::Bot;

use crate::binder::SessionBinder;
use crate::cancel::CancelRegistry;
use crate::config::GatewayConfig;
use crate::daemon::RealConnFactory;
use crate::orchestrator::OrchestratorConfig;
use crate::telegram::{run as telegram_run, BotDeps, TeloxideReplier};

/// CLI arguments for the gateway entry point. Used both by the standalone
/// `roy-gateway` binary and by the `roy gateway` subcommand of `roy-cli`.
#[derive(clap::Parser, Debug)]
#[command(name = "roy-gateway")]
pub struct Args {
    /// Path to the gateway TOML config.
    #[arg(long)]
    pub config: PathBuf,
}

/// Run the gateway: load config, spawn whichever adapters (Telegram, WS) the
/// config enables, and wait for the first one to exit. Does NOT install a
/// tracing subscriber — the caller owns that.
pub async fn run(args: Args) -> Result<()> {
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
    Ok(Some(tokio::spawn(
        async move { telegram_run(bot, deps).await },
    )))
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
