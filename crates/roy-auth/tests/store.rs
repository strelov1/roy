use sqlx::SqlitePool;

async fn fresh_pool() -> SqlitePool {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents.db");
    std::mem::forget(dir);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    roy_auth::apply_migrations(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn migration_creates_users_table() {
    let pool = fresh_pool().await;
    let tables: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name='users'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(tables.len(), 1);
}
