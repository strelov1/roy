use std::path::PathBuf;
use std::sync::Arc;

use sqlx::SqlitePool;

use crate::meta_store::MetaStore;
use crate::roy_client::DaemonClient;

/// Shared handler state. Cloneable: `PathBuf` is cheap to clone, `MetaStore`
/// wraps an `Arc`'d pool, `Arc<dyn DaemonClient>` is always Clone.
#[derive(Clone)]
pub struct AppState {
    pub meta: MetaStore,
    pub daemon: Arc<dyn DaemonClient>,
    /// Path to the roy daemon's Unix socket (for spawning sessions).
    pub socket_path: PathBuf,
    /// Read-only handle to `roy-scheduler`'s SQLite DB. `None` if the
    /// scheduler DB doesn't exist yet — scheduler endpoints respond 503
    /// in that case so the UI can show a "scheduler not initialized" notice
    /// instead of a generic 500.
    pub scheduler_pool: Option<SqlitePool>,
    /// Shared sqlite pool — needed by roy-auth middleware/handlers and ACL.
    pub pool: SqlitePool,
    /// Workspace root for resolve_cwd (Phase C).
    pub workspace_dir: PathBuf,
    /// In-memory token-bucket rate limiter for `POST /auth/login`. Process-
    /// global state; resets on restart. Shared per-`AppState` via `Arc` so the
    /// `Clone` derive on `AppState` keeps all clones pointing at the same
    /// buckets.
    pub login_limiter: Arc<crate::rate_limit::LoginLimiter>,
    /// 30s TTL cache for filesystem-discovered slash commands. Shared via
    /// `Arc` so all `AppState` clones see the same cache state.
    pub commands_cache: Arc<crate::commands::CommandsCache>,
    /// 30s TTL cache for filesystem-discovered agent files. Shared via
    /// `Arc` so all `AppState` clones see the same cache state.
    pub agents_cache: Arc<crate::agents::AgentsCache>,
    /// User-owned MCP connections store. Wraps an `Arc`'d pool so cloning
    /// `AppState` is cheap.
    pub connections: crate::connections::Store,
    /// Read-only provider catalog loaded from `~/.roy/connections.yaml` at
    /// boot. Cloneable because `Arc<Catalog>` is. Empty for users without
    /// a yaml file.
    pub catalog: std::sync::Arc<crate::provider_catalog::Catalog>,
    /// Channel→agent binding store. Wraps an `Arc`'d pool so cloning
    /// `AppState` is cheap.
    pub channel_bindings: crate::channel_bindings::Store,
    /// Bearer token gating `GET /internal/telegram-sources`. `None` disables it.
    pub internal_token: Option<String>,
}
