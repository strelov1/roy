//! Migration loader for roy-auth. The `users`, `teams`, `team_members`, and
//! `team_invites` tables share the `agents.db` SQLite file with
//! roy-management; both crates' migrations live side-by-side in the same
//! `_sqlx_migrations` table, with `set_ignore_missing(true)` so each
//! migrator tolerates rows owned by the other.

use sqlx::SqlitePool;

pub async fn apply_migrations(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    let mut migrator = sqlx::migrate!("migrations/sqlite");
    migrator.set_ignore_missing(true);
    migrator.run(pool).await
}
