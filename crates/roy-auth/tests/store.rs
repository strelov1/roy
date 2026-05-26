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

use roy_auth::{NewUser, UserStore, UserStoreError};

#[tokio::test]
async fn create_user_then_lookup() {
    let pool = fresh_pool().await;
    let store = UserStore::new(pool);
    let user = store
        .create(NewUser {
            username: "alice".into(),
            display_name: "Alice".into(),
            password: "correcthorsebattery".into(),
            timezone: None,
        })
        .await
        .unwrap();
    assert_eq!(user.username, "alice");
    assert_ne!(user.password_hash, "correcthorsebattery"); // hashed

    let by_id = store.get(&user.id).await.unwrap();
    assert_eq!(by_id.username, "alice");
    let by_name = store.get_by_username("ALICE").await.unwrap(); // COLLATE NOCASE
    assert_eq!(by_name.id, user.id);
    assert!(store.has_any().await.unwrap());
}

#[tokio::test]
async fn duplicate_username_rejected() {
    let pool = fresh_pool().await;
    let store = UserStore::new(pool);
    let mk = || NewUser {
        username: "alice".into(),
        display_name: "A".into(),
        password: "12345678".into(),
        timezone: None,
    };
    store.create(mk()).await.unwrap();
    let err = store.create(mk()).await.unwrap_err();
    assert!(matches!(err, UserStoreError::UsernameTaken));
}

#[tokio::test]
async fn short_password_rejected() {
    let pool = fresh_pool().await;
    let store = UserStore::new(pool);
    let err = store
        .create(NewUser {
            username: "bob".into(),
            display_name: "B".into(),
            password: "short".into(),
            timezone: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, UserStoreError::Invalid(_)));
}

#[test]
fn hash_and_verify_round_trip() {
    let hash = roy_auth::hash_password("hunter22-correct").unwrap();
    assert!(roy_auth::verify_password("hunter22-correct", &hash).unwrap());
    assert!(!roy_auth::verify_password("wrong", &hash).unwrap());
}

use roy_auth::{NewTeam, TeamStore};

#[tokio::test]
async fn create_team_lists_owner() {
    let pool = fresh_pool().await;
    let users = UserStore::new(pool.clone());
    let alice = users
        .create(NewUser {
            username: "alice".into(),
            display_name: "A".into(),
            password: "12345678".into(),
            timezone: None,
        })
        .await
        .unwrap();
    let teams = TeamStore::new(pool.clone());
    let team = teams
        .create(
            NewTeam {
                name: "eng".into(),
                description: None,
            },
            &alice.id,
        )
        .await
        .unwrap();

    let memberships = teams.list_for_user(&alice.id).await.unwrap();
    assert_eq!(memberships.len(), 1);
    assert_eq!(memberships[0].id, team.id);
    assert_eq!(memberships[0].name, "eng");
    assert!(teams.is_owner(&alice.id, &team.id).await.unwrap());
    assert!(teams.is_member(&alice.id, &team.id).await.unwrap());

    // Bob is not a member.
    let bob = users
        .create(NewUser {
            username: "bob".into(),
            display_name: "B".into(),
            password: "12345678".into(),
            timezone: None,
        })
        .await
        .unwrap();
    assert!(!teams.is_member(&bob.id, &team.id).await.unwrap());
}
