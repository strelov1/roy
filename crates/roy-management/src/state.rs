use std::path::PathBuf;
use std::sync::Arc;

use roy_agents::Store;
use sqlx::SqlitePool;

use crate::meta_store::MetaStore;
use crate::roy_client::DaemonClient;

/// Shared handler state. Cloneable: `Store` wraps an `Arc`'d pool, `PathBuf` is
/// cheap to clone, `MetaStore` wraps an `Arc`'d pool, `Arc<dyn DaemonClient>`
/// is always Clone.
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub meta: MetaStore,
    pub daemon: Arc<dyn DaemonClient>,
    /// Path to the roy daemon's Unix socket (for spawning sessions).
    pub socket_path: PathBuf,
    /// Read-only handle to `roy-scheduler`'s SQLite DB. `None` if the
    /// scheduler DB doesn't exist yet — scheduler endpoints respond 503
    /// in that case so the UI can show a "scheduler not initialized" notice
    /// instead of a generic 500.
    pub scheduler_pool: Option<SqlitePool>,
}
