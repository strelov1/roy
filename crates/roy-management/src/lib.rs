//! roy-management library: agent CRUD HTTP service over the daemon socket.
//! The bin is a thin clap-driven entrypoint over these modules; integration
//! tests link this library directly to exercise the real wire code paths.

pub mod agents;
pub mod auth;
pub mod bootstrap;
pub mod commands;
pub mod connections;
pub mod cwd;
pub mod db;
pub mod http;
pub mod meta_store;
pub mod orphan_sweep;
pub mod provider_catalog;
pub mod rate_limit;
pub mod roy_client;
pub mod state;
pub mod uploads;

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::state::AppState;

#[derive(clap::Parser, Debug)]
#[command(name = "roy-management", about = "Agent store + HTTP API for roy")]
pub struct Args {
    /// Address to bind the HTTP server to.
    #[arg(long, env = "ROY_MANAGEMENT_ADDR", default_value = "127.0.0.1:8079")]
    pub addr: SocketAddr,
    /// Path to the agents SQLite DB. Defaults to ~/.local/state/roy/agents.db.
    #[arg(long, env = "ROY_AGENTS_DB")]
    pub db: Option<PathBuf>,
    /// roy daemon Unix socket. Defaults to $ROY_SOCKET or ~/.roy/daemon.sock.
    #[arg(long, env = "ROY_SOCKET")]
    pub socket: Option<PathBuf>,
}

/// Build and serve the management HTTP API.
pub async fn run(args: Args) -> anyhow::Result<()> {
    use std::sync::Arc;

    // Fail fast on a misconfigured JWT secret before touching any SQLite
    // files. `secret_from_env` returns the secret string but we throw it
    // away here — each `/auth/login` call re-reads it from env.
    roy_auth::jwt::secret_from_env()
        .map_err(|e| anyhow::anyhow!("ROY_JWT_SECRET missing or shorter than 32 bytes: {e}"))?;

    let db_path = args.db.unwrap_or_else(crate::db::default_db_path);
    let pool = crate::db::open(&db_path).await?;

    meta_store::MetaStore::apply_migrations(&pool).await?;
    roy_auth::apply_migrations(&pool).await?;
    bootstrap::ensure_root(&pool).await?;
    let socket = args.socket.unwrap_or_else(default_socket);
    let workspace_dir = meta_store::default_workspace_dir();
    std::fs::create_dir_all(&workspace_dir)?;
    let meta = meta_store::MetaStore::new(pool.clone(), workspace_dir.clone());
    let daemon: Arc<dyn roy_client::DaemonClient> =
        Arc::new(roy_client::UnixSocketDaemonClient::new(socket.clone()));

    // We don't create the scheduler DB file from this process — that's
    // roy-scheduler's job. The exists() guard prevents sqlx's
    // create_if_missing default from materializing an empty (un-migrated) DB
    // that would then permanently shadow the real one.
    let scheduler_db_path = roy_scheduler::default_db_path();
    let scheduler_pool = if scheduler_db_path.exists() {
        match roy_scheduler::db::open(&scheduler_db_path).await {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, db = %scheduler_db_path.display(), "scheduler DB present but failed to open");
                None
            }
        }
    } else {
        None
    };

    let catalog_path = crate::provider_catalog::default_path();
    let catalog = match crate::provider_catalog::Catalog::load_from(&catalog_path) {
        Ok(c) => {
            tracing::info!(
                path = %catalog_path.display(),
                providers = c.providers().len(),
                "provider catalog loaded"
            );
            std::sync::Arc::new(c)
        }
        Err(e) => {
            anyhow::bail!(
                "provider catalog at {} is malformed: {e}. Fix the file or \
                remove it to use an empty catalog. Reference sample at: \
                crates/roy-management/resources/connections.default.yaml",
                catalog_path.display()
            );
        }
    };

    let state = AppState {
        meta,
        daemon,
        socket_path: socket,
        scheduler_pool,
        connections: crate::connections::Store::new(pool.clone()),
        catalog,
        pool,
        workspace_dir,
        login_limiter: Arc::new(crate::rate_limit::LoginLimiter::default()),
        commands_cache: Arc::new(crate::commands::CommandsCache::default()),
        agents_cache: Arc::new(crate::agents::AgentsCache::default()),
    };

    orphan_sweep::spawn(state.meta.clone(), Arc::clone(&state.daemon));

    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind(args.addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!(
        addr = %bound,
        db = %db_path.display(),
        "listening on {}",
        bound
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}
