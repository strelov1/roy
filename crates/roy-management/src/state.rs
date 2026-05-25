use std::path::PathBuf;

use roy_agents::Store;

/// Shared handler state. Cloneable: `Store` wraps an `Arc`'d pool, `PathBuf` is
/// cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    /// Path to the roy daemon's Unix socket (for spawning sessions).
    pub socket_path: PathBuf,
}
