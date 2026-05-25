//! SQLite pool + auto-migrate for the shared agent store. WAL mode (so the
//! daemon, scheduler, and management can share the file), 5s busy timeout,
//! mode 0600 (prompts may be sensitive). The `_sqlx_migrations` table is
//! shared with `roy-management` (which owns v2 onward), so this crate's
//! migrator runs with `set_ignore_missing(true)` to tolerate rows it
//! doesn't own.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

pub async fn open(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("opening SQLite at {}", path.display()))?;
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

/// `$ROY_AGENTS_DB`, else `~/.local/state/roy/agents.db`.
pub fn default_db_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("ROY_AGENTS_DB") {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".local/state/roy/agents.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn open_creates_db_and_applies_migration() {
        let dir = tempdir().unwrap();
        let pool = open(&dir.path().join("agents.db")).await.unwrap();
        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name='agents'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(tables.len(), 1, "agents table created by migration");
    }
}
