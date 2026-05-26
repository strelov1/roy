//! Shared test helpers — gated behind the `test-support` feature so they can be
//! consumed by sibling crates without leaking into release builds.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::team_store::TeamStore;
use crate::types::{NewTeam, NewUser, Team, User};
use crate::user_store::UserStore;

pub const TEST_JWT_SECRET: &str = "roy-test-jwt-secret-32-chars-min!!";

pub async fn temp_pool() -> SqlitePool {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agents.db");
    std::mem::forget(dir);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("sqlite connect");
    crate::db::apply_migrations(&pool)
        .await
        .expect("migrations");
    pool
}

pub async fn make_user(pool: &SqlitePool, username: &str) -> User {
    UserStore::new(pool.clone())
        .create(NewUser {
            username: username.into(),
            display_name: username.into(),
            password: "test-password-1234".into(),
            timezone: None,
        })
        .await
        .expect("make_user")
}

pub fn issue_jwt(user_id: &str) -> String {
    crate::jwt::sign_session(user_id, TEST_JWT_SECRET, 3600).expect("sign jwt")
}

pub async fn make_team(pool: &SqlitePool, owner_id: &str, name: &str) -> Team {
    TeamStore::new(pool.clone())
        .create(
            NewTeam {
                name: name.into(),
                description: None,
            },
            owner_id,
        )
        .await
        .expect("make_team")
}
