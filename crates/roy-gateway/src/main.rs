use std::collections::HashSet;
use std::path::PathBuf;
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

    let binder_path = PathBuf::from(&cfg.binder.path);
    let binder = Arc::new(
        SessionBinder::load(binder_path.clone())
            .await
            .with_context(|| format!("loading binder {}", binder_path.display()))?,
    );

    let conn_factory = Arc::new(RealConnFactory::new(socket_path));

    let orch_cfg = Arc::new(OrchestratorConfig {
        preset: cfg.telegram.preset.clone(),
        project_id: cfg.telegram.project_id.clone(),
        turn_timeout: Duration::from_secs(cfg.telegram.turn_timeout_secs),
        typing_interval: Duration::from_secs(4),
    });

    let bot = Bot::new(cfg.telegram.token);
    let replier = Arc::new(TeloxideReplier::new(bot.clone()));

    let allowed: HashSet<u64> = cfg.telegram.allowed_user_ids.iter().copied().collect();
    let deps = BotDeps {
        cfg: orch_cfg,
        binder,
        conn_factory,
        replier,
        cancel_registry: CancelRegistry::new(),
        allowed_user_ids: Arc::new(allowed),
    };

    run(bot, deps).await
}
