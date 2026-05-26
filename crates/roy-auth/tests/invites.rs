use roy_auth::{InviteError, InviteStore, NewTeam, NewUser, TeamStore, UserStore};
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
async fn accept_invite_adds_member() {
    let pool = fresh_pool().await;
    let alice = UserStore::new(pool.clone())
        .create(NewUser {
            username: "alice".into(),
            display_name: "A".into(),
            password: "12345678".into(),
            timezone: None,
        })
        .await
        .unwrap();
    let bob = UserStore::new(pool.clone())
        .create(NewUser {
            username: "bob".into(),
            display_name: "B".into(),
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

    let invites = InviteStore::new(pool.clone());
    let inv = invites.create(&team.id, &alice.id, None).await.unwrap();

    let tid = invites.accept(&inv.token, &bob.id).await.unwrap();
    assert_eq!(tid, team.id);
    assert!(teams.is_member(&bob.id, &team.id).await.unwrap());
}

#[tokio::test]
async fn consumed_invite_rejected() {
    let pool = fresh_pool().await;
    let alice = UserStore::new(pool.clone())
        .create(NewUser {
            username: "alice".into(),
            display_name: "A".into(),
            password: "12345678".into(),
            timezone: None,
        })
        .await
        .unwrap();
    let team = TeamStore::new(pool.clone())
        .create(
            NewTeam {
                name: "eng".into(),
                description: None,
            },
            &alice.id,
        )
        .await
        .unwrap();
    let invites = InviteStore::new(pool.clone());
    let inv = invites.create(&team.id, &alice.id, None).await.unwrap();
    invites.accept(&inv.token, &alice.id).await.unwrap();
    assert!(matches!(
        invites.accept(&inv.token, &alice.id).await,
        Err(InviteError::Invalid)
    ));
}

#[tokio::test]
async fn expired_invite_rejected() {
    let pool = fresh_pool().await;
    let alice = UserStore::new(pool.clone())
        .create(NewUser {
            username: "alice".into(),
            display_name: "A".into(),
            password: "12345678".into(),
            timezone: None,
        })
        .await
        .unwrap();
    let team = TeamStore::new(pool.clone())
        .create(
            NewTeam {
                name: "eng".into(),
                description: None,
            },
            &alice.id,
        )
        .await
        .unwrap();
    let invites = InviteStore::new(pool.clone());
    let inv = invites.create(&team.id, &alice.id, Some(0)).await.unwrap(); // expires_at = epoch → expired
    assert!(matches!(
        invites.accept(&inv.token, &alice.id).await,
        Err(InviteError::Invalid)
    ));
}

#[tokio::test]
async fn bad_token_rejected() {
    let pool = fresh_pool().await;
    let alice = UserStore::new(pool.clone())
        .create(NewUser {
            username: "alice".into(),
            display_name: "A".into(),
            password: "12345678".into(),
            timezone: None,
        })
        .await
        .unwrap();
    let invites = InviteStore::new(pool.clone());
    assert!(matches!(
        invites.accept("nonexistent-token", &alice.id).await,
        Err(InviteError::Invalid)
    ));
}
