//! Migration loader for roy-auth. Shares the sqlx `_sqlx_migrations` table with
//! roy-agents (v1-3) and roy-management (v4-9). Runs with
//! `set_ignore_missing(true)` so we tolerate rows owned by sibling crates.

use sqlx::SqlitePool;

pub async fn apply_migrations(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    let mut migrator = sqlx::migrate!("migrations/sqlite");
    migrator.set_ignore_missing(true);
    migrator.run(pool).await
}
