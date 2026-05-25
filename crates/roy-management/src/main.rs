mod http;
mod roy_client;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

use crate::state::AppState;

#[derive(Parser, Debug)]
#[command(name = "roy-management", about = "Agent store + HTTP API for roy")]
struct Args {
    /// Address to bind the HTTP server to.
    #[arg(long, env = "ROY_MANAGEMENT_ADDR", default_value = "127.0.0.1:8079")]
    addr: SocketAddr,
    /// Path to the agents SQLite DB. Defaults to ~/.local/state/roy/agents.db.
    #[arg(long, env = "ROY_AGENTS_DB")]
    db: Option<PathBuf>,
    /// roy daemon Unix socket. Defaults to $ROY_SOCKET or ~/.roy/daemon.sock.
    #[arg(long, env = "ROY_SOCKET")]
    socket: Option<PathBuf>,
}

/// Matches roy-scheduler's `default_socket` so both find the same daemon.
fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "roy_management=info,warn".into()),
        )
        .init();

    let args = Args::parse();
    let db_path = args.db.unwrap_or_else(roy_agents::default_db_path);
    let pool = roy_agents::open(&db_path).await?;
    let state = AppState {
        store: roy_agents::Store::new(pool),
        socket_path: args.socket.unwrap_or_else(default_socket),
    };

    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind(args.addr).await?;
    tracing::info!(
        addr = %args.addr,
        db = %db_path.display(),
        "roy-management listening"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
