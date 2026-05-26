//! Idempotent bootstrap: if the users table is empty, create a `root` user
//! using `ROY_BOOTSTRAP_USERNAME` / `ROY_BOOTSTRAP_PASSWORD` (or a printed
//! random password). Returns whether the user was just created.

use rand::RngCore;
use roy_auth::{NewUser, UserStore};
use sqlx::SqlitePool;

pub async fn ensure_root(pool: &SqlitePool) -> anyhow::Result<bool> {
    let store = UserStore::new(pool.clone());
    if store.has_any().await? {
        return Ok(false);
    }
    let username = std::env::var("ROY_BOOTSTRAP_USERNAME").unwrap_or_else(|_| "root".into());
    let display_name = std::env::var("USER").unwrap_or_else(|_| username.clone());
    let password = match std::env::var("ROY_BOOTSTRAP_PASSWORD") {
        Ok(s) => s,
        Err(_) => {
            let mut buf = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut buf);
            let pw = hex::encode(buf);
            eprintln!("roy: bootstrap user {username:?} — password: {pw}");
            pw
        }
    };
    store
        .create(NewUser {
            username,
            display_name,
            password,
            timezone: None,
        })
        .await?;
    Ok(true)
}
