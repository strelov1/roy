// crates/roy-management/src/db.rs
//
// Shared SQLite helpers for roy-management. The `default_db_path` and `open`
// functions were previously in the roy-agents crate; that crate is being
// deleted now that agents live in `~/.roy/agents/*.md` files.

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

/// Open (or create) the roy SQLite DB at `path`. WAL mode, 5s busy timeout,
/// mode 0600 on Unix. Runs roy-management's own migrator with
/// `set_ignore_missing(true)` so existing deployments with older rows aren't
/// broken.
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
    // `set_ignore_missing(true)` so we don't error on migration rows owned by
    // other crates that may have been applied before this binary ran.
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
