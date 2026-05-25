use std::path::PathBuf;
use std::sync::Arc;

use roy_agents::Store;

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
}
