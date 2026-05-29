//! Shared SQLite pool helpers for the `agents.db` file. Used by every
//! roy-management consumer (HTTP service, tests) and shared with `roy-auth`,
//! which adds its own tables via its own migrator.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

/// `$ROY_AGENTS_DB`, else `~/.local/state/roy/agents.db`.
pub fn default_db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ROY_AGENTS_DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy/agents.db")
}

/// Open (or create) the SQLite pool at `path`, apply roy-management's
/// migrations, and chmod the file to `0600` on Unix. `roy-auth` shares this
/// pool and adds its own tables on top via [`roy_auth::apply_migrations`].
pub async fn open(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5))
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("opening SQLite at {}", path.display()))?;
    // `set_ignore_missing(true)` so we tolerate migration rows owned by
    // roy-auth that may have been applied earlier into the same database.
    let mut migrator = sqlx::migrate!("migrations/sqlite");
    migrator.set_ignore_missing(true);
    migrator.run(&pool).await.context("running migrations")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(pool)
}
