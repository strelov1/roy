//! roy-management library: agent CRUD HTTP service over the daemon socket.
//! The bin is a thin clap-driven entrypoint over these modules; integration
//! tests link this library directly to exercise the real wire code paths.

pub mod http;
pub mod meta_store;
pub mod orphan_sweep;
pub mod roy_client;
pub mod state;

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

    let db_path = args.db.unwrap_or_else(roy_agents::default_db_path);
    let pool = roy_agents::open(&db_path).await?;

    meta_store::MetaStore::apply_migrations(&pool).await?;
    let socket = args.socket.unwrap_or_else(default_socket);
    let meta = meta_store::MetaStore::new(pool.clone());
    let daemon: Arc<dyn roy_client::DaemonClient> =
        Arc::new(roy_client::UnixSocketDaemonClient::new(socket.clone()));

    let state = AppState {
        store: roy_agents::Store::new(pool),
        meta,
        daemon,
        socket_path: socket,
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
    axum::serve(listener, app).await?;
    Ok(())
}

fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}
