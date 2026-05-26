# User Auth + Per-Scope cwd + Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-05-25-user-auth-commands-design.md`

**Goal:** Add users, teams, JWT cookie auth, per-scope cwd layout, and `~/.claude/skills` slash-command discovery to roy without changing the daemon's wire protocol.

**Architecture:** New library crate `roy-auth` owns user/team/invite tables + JWT/bcrypt + ACL helpers in the shared `agents.db`. `roy-management` adds an axum middleware that resolves JWT cookie → `user_id` and wires it into every existing handler. `roy-gateway` switches its WS handshake from shared-UUID to JWT subprotocol verification. `roy` daemon stays trusted — HTTP/WS layer resolves absolute `cwd` and `user_id` before any `ClientCommand::Spawn`.

**Tech Stack:** Rust 2021, sqlx 0.8 (sqlite + WAL), axum 0.8, tokio, `bcrypt` crate, `jsonwebtoken` crate (HS256), `uuid` v4, `chrono`. Existing testing fixture: in-memory SQLite via `tempfile::tempdir`.

**Execution order:** Phases A → G strictly sequential. Each phase ends with `cargo test --workspace --no-fail-fast` green. Within a phase, tasks must be done in order.

---

## File map

**New files:**
- `crates/roy-auth/Cargo.toml`
- `crates/roy-auth/src/lib.rs`
- `crates/roy-auth/src/db.rs`              ─ migration loader
- `crates/roy-auth/src/types.rs`           ─ `User`, `Team`, `TeamMember`, `TeamInvite`, `Role`, `Scope`, `UserProfile`, `TeamMembership`
- `crates/roy-auth/src/user_store.rs`      ─ user CRUD
- `crates/roy-auth/src/team_store.rs`      ─ team CRUD + membership
- `crates/roy-auth/src/invite_store.rs`    ─ invite create/accept
- `crates/roy-auth/src/password.rs`        ─ bcrypt wrapper + dummy hash
- `crates/roy-auth/src/jwt.rs`             ─ sign/verify, secret loader
- `crates/roy-auth/src/cookie.rs`          ─ cookie parse + verify_cookie/verify_ws_protocol
- `crates/roy-auth/src/acl.rs`             ─ `Acl` struct
- `crates/roy-auth/src/test_support.rs`    ─ `temp_pool`, `make_user`, `make_team`, `issue_jwt`
- `crates/roy-auth/migrations/sqlite/0010_users.sql`
- `crates/roy-auth/migrations/sqlite/0011_teams.sql`
- `crates/roy-auth/migrations/sqlite/0012_team_invites.sql`
- `crates/roy-auth/tests/jwt.rs`
- `crates/roy-auth/tests/store.rs`
- `crates/roy-auth/tests/invites.rs`
- `crates/roy-management/src/auth.rs`      ─ axum middleware + handlers + extractors
- `crates/roy-management/src/cwd.rs`       ─ `resolve_cwd`, `require_safe_path`
- `crates/roy-management/src/commands.rs`  ─ skill scanner
- `crates/roy-management/src/rate_limit.rs` ─ in-memory IP token-bucket
- `crates/roy-management/src/bootstrap.rs` ─ bootstrap-root
- `crates/roy-management/migrations/sqlite/0005_owners.sql`
- `crates/roy-management/tests/auth_flow.rs`
- `crates/roy-management/tests/acl.rs`
- `crates/roy-management/tests/session_cwd.rs`
- `crates/roy-management/tests/commands_discovery.rs`
- `crates/roy-gateway/tests/ws_auth.rs`
- `crates/roy-cli/src/auth.rs`

**Modified files:**
- `Cargo.toml`                                ─ workspace stays unchanged (`members = ["crates/*"]` picks up new crate)
- `crates/roy-management/Cargo.toml`         ─ add `roy-auth`, `bcrypt`, `jsonwebtoken`, `tower-http`, `axum-extra`
- `crates/roy-management/src/lib.rs`         ─ call `roy_auth::apply_migrations`, `bootstrap::ensure_root`
- `crates/roy-management/src/state.rs`       ─ add `pub pool: SqlitePool`
- `crates/roy-management/src/http.rs`        ─ mount `/auth/*`, `/teams`, `/commands`; layer middleware
- `crates/roy-management/src/meta_store.rs`  ─ extend `Project`/`SessionMeta` with `created_by`/`team_id`; new `create_project_v2`, `upsert_session_meta_v2`
- `crates/roy-gateway/Cargo.toml`            ─ add `roy-auth`, drop `uuid` v4 minting code path
- `crates/roy-gateway/src/lib.rs`            ─ pass `pool` into `ws::serve`
- `crates/roy-gateway/src/ws.rs`             ─ rewrite `ws_auth_callback`
- `crates/roy-cli/Cargo.toml`                ─ add `reqwest`, `rpassword`
- `crates/roy-cli/src/main.rs`               ─ add `roy auth` subcommand tree

---

## Phase A — `roy-auth` foundation crate

### Task A1: Scaffold the `roy-auth` crate

**Files:**
- Create: `crates/roy-auth/Cargo.toml`
- Create: `crates/roy-auth/src/lib.rs`

- [ ] **Step 1: Create `crates/roy-auth/Cargo.toml`**

```toml
[package]
name = "roy-auth"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[features]
default = []
test-support = []

[dependencies]
sqlx = { version = "0.8", default-features = false, features = [
  "runtime-tokio",
  "sqlite",
  "chrono",
  "macros",
  "migrate",
] }
chrono = { version = "0.4", default-features = false, features = ["serde", "clock"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
uuid = { version = "1", features = ["v4", "serde"] }
bcrypt = "0.16"
jsonwebtoken = { version = "9", default-features = false }
hex = "0.4"
rand = "0.8"
tracing = "0.1"

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["full"] }
```

- [ ] **Step 2: Create `crates/roy-auth/src/lib.rs`**

```rust
//! User/team/invite store + JWT helpers shared by roy-management and roy-gateway.
//! Tables live in the shared `agents.db` next to roy-agents and roy-management
//! (migration versions 10+). The crate exposes a small surface: stores, JWT
//! sign/verify, cookie parsing, and an `Acl` helper.

pub mod acl;
pub mod cookie;
pub mod db;
pub mod invite_store;
pub mod jwt;
pub mod password;
pub mod team_store;
pub mod types;
pub mod user_store;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use acl::{Acl, AclError};
pub use cookie::{verify_cookie, verify_ws_protocol, COOKIE_NAME};
pub use db::apply_migrations;
pub use invite_store::{InviteStore, InviteError};
pub use jwt::{sign_session, verify_session, JwtError};
pub use password::{hash_password, verify_password};
pub use team_store::{TeamStore, TeamStoreError};
pub use types::{
    NewTeam, NewUser, Role, Scope, Team, TeamInvite, TeamMember, TeamMembership, User,
    UserProfile,
};
pub use user_store::{UserStore, UserStoreError};
```

- [ ] **Step 3: Verify the crate builds (no implementations yet — will fail with missing modules)**

Run: `cargo build -p roy-auth 2>&1 | head -40`
Expected: Compile errors `file not found for module 'acl'` etc. — confirms cargo picked up the new crate. We resolve each module in the following tasks.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-auth/Cargo.toml crates/roy-auth/src/lib.rs
git commit -m "chore(roy-auth): scaffold new library crate"
```

---

### Task A2: Migration 0010 — users table

**Files:**
- Create: `crates/roy-auth/migrations/sqlite/0010_users.sql`
- Create: `crates/roy-auth/src/db.rs`

- [ ] **Step 1: Write the migration**

`crates/roy-auth/migrations/sqlite/0010_users.sql`:
```sql
CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    timezone      TEXT,
    created_at    INTEGER NOT NULL
);
```

- [ ] **Step 2: Write the migration loader**

`crates/roy-auth/src/db.rs`:
```rust
//! Migration loader for roy-auth. Shares the sqlx `_sqlx_migrations` table with
//! roy-agents (v1-3) and roy-management (v4-9). Runs with
//! `set_ignore_missing(true)` so we tolerate rows owned by sibling crates.

use sqlx::SqlitePool;

pub async fn apply_migrations(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    let mut migrator = sqlx::migrate!("migrations/sqlite");
    migrator.set_ignore_missing(true);
    migrator.run(pool).await
}
```

- [ ] **Step 3: Write a smoke test**

`crates/roy-auth/tests/store.rs` (initial content — will be extended later):
```rust
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
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='users'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(tables.len(), 1);
}
```

- [ ] **Step 4: Stub remaining modules so the crate compiles**

Create these as empty stub files (each phase fills them in):
- `crates/roy-auth/src/types.rs`         → `pub struct User;` and friends — see Task A3
- `crates/roy-auth/src/user_store.rs`    → `pub struct UserStore;` — see Task A3
- `crates/roy-auth/src/team_store.rs`    → `pub struct TeamStore;` — see Task A7
- `crates/roy-auth/src/invite_store.rs`  → `pub struct InviteStore;` — see Task A9
- `crates/roy-auth/src/password.rs`      → see Task A4
- `crates/roy-auth/src/jwt.rs`           → see Task A5
- `crates/roy-auth/src/cookie.rs`        → see Task A12
- `crates/roy-auth/src/acl.rs`           → see Task A10

For now, write minimal placeholders that re-export at least the items in `lib.rs`. Example for `types.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewUser;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTeam;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMembership;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamInvite;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role { Owner, Member }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Scope { Personal, Team(String) }
```

Equivalent placeholders for the remaining files. They will be expanded in later tasks; we just need the crate to compile end-to-end after Task A2.

- [ ] **Step 5: Run the migration test**

Run: `cargo test -p roy-auth --test store migration_creates_users_table -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): add users migration (0010) + skeleton modules"
```

---

### Task A3: `User` type + `UserStore` (create/get/has_any)

**Files:**
- Modify: `crates/roy-auth/src/types.rs`
- Modify: `crates/roy-auth/src/user_store.rs`
- Modify: `crates/roy-auth/tests/store.rs`

- [ ] **Step 1: Replace `User`/`NewUser` placeholders in `types.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub timezone: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewUser {
    pub username: String,
    pub display_name: String,
    pub password: String,            // plaintext at this boundary; hashed inside the store
    #[serde(default)]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub timezone: Option<String>,
    pub teams: Vec<crate::types::TeamMembership>,
}

// Team* and Role placeholders stay; replaced in Task A7.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Team { pub id: String }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTeam { pub name: String }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamMember;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamMembership { pub id: String, pub name: String, pub role: Role }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamInvite { pub token: String }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Owner, Member }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "lowercase")]
pub enum Scope {
    Personal,
    Team { team_id: String },
}
```

- [ ] **Step 2: Write `UserStore`**

`crates/roy-auth/src/user_store.rs`:
```rust
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::password::hash_password;
use crate::types::{NewUser, User};

#[derive(Debug, thiserror::Error)]
pub enum UserStoreError {
    #[error("user not found: {0}")]
    NotFound(String),
    #[error("username already exists")]
    UsernameTaken,
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("bcrypt: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
}

#[derive(Clone)]
pub struct UserStore {
    pool: SqlitePool,
}

impl UserStore {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn create(&self, new: NewUser) -> Result<User, UserStoreError> {
        if new.username.trim().is_empty() {
            return Err(UserStoreError::Invalid("username required".into()));
        }
        if new.password.len() < 8 {
            return Err(UserStoreError::Invalid("password too short (min 8)".into()));
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp_millis();
        let hash = hash_password(&new.password)?;
        let res = sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash, timezone, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&new.username)
        .bind(&new.display_name)
        .bind(&hash)
        .bind(&new.timezone)
        .bind(now)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(User {
                id,
                username: new.username,
                display_name: new.display_name,
                password_hash: hash,
                timezone: new.timezone,
                created_at: now,
            }),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                Err(UserStoreError::UsernameTaken)
            }
            Err(e) => Err(UserStoreError::Db(e)),
        }
    }

    pub async fn get(&self, id: &str) -> Result<User, UserStoreError> {
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| UserStoreError::NotFound(id.into()))
    }

    pub async fn get_by_username(&self, username: &str) -> Result<User, UserStoreError> {
        sqlx::query_as::<_, User>(
            "SELECT * FROM users WHERE username = ? COLLATE NOCASE",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| UserStoreError::NotFound(username.into()))
    }

    pub async fn has_any(&self) -> Result<bool, UserStoreError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM users LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn set_password(&self, user_id: &str, new_password: &str) -> Result<(), UserStoreError> {
        if new_password.len() < 8 {
            return Err(UserStoreError::Invalid("password too short (min 8)".into()));
        }
        let hash = hash_password(new_password)?;
        let res = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(&hash)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(UserStoreError::NotFound(user_id.into()));
        }
        Ok(())
    }

    pub async fn set_timezone(&self, user_id: &str, tz: Option<&str>) -> Result<(), UserStoreError> {
        sqlx::query("UPDATE users SET timezone = ? WHERE id = ?")
            .bind(tz)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
```

- [ ] **Step 3: Add unit tests**

Append to `crates/roy-auth/tests/store.rs`:
```rust
use roy_auth::{NewUser, UserStore, UserStoreError};

#[tokio::test]
async fn create_user_then_lookup() {
    let pool = fresh_pool().await;
    let store = UserStore::new(pool);
    let user = store.create(NewUser {
        username: "alice".into(),
        display_name: "Alice".into(),
        password: "correcthorsebattery".into(),
        timezone: None,
    }).await.unwrap();
    assert_eq!(user.username, "alice");
    assert_ne!(user.password_hash, "correcthorsebattery"); // hashed

    let by_id = store.get(&user.id).await.unwrap();
    assert_eq!(by_id.username, "alice");
    let by_name = store.get_by_username("ALICE").await.unwrap();   // COLLATE NOCASE
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
    let err = store.create(NewUser {
        username: "bob".into(),
        display_name: "B".into(),
        password: "short".into(),
        timezone: None,
    }).await.unwrap_err();
    assert!(matches!(err, UserStoreError::Invalid(_)));
}
```

- [ ] **Step 4: Run the tests (will fail — `hash_password` not implemented)**

Run: `cargo test -p roy-auth --test store -- --nocapture`
Expected: FAIL — Task A4 introduces `password.rs`.

- [ ] **Step 5: Defer commit until Task A4 lands `password.rs`.**

---

### Task A4: `password.rs` — bcrypt wrapper

**Files:**
- Modify: `crates/roy-auth/src/password.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/roy-auth/tests/store.rs`:
```rust
#[test]
fn hash_and_verify_round_trip() {
    let hash = roy_auth::hash_password("hunter22-correct").unwrap();
    assert!(roy_auth::verify_password("hunter22-correct", &hash).unwrap());
    assert!(!roy_auth::verify_password("wrong", &hash).unwrap());
}
```

- [ ] **Step 2: Run test (fails on `hash_password` unimplemented)**

Run: `cargo test -p roy-auth --test store hash_and_verify_round_trip -- --nocapture`
Expected: FAIL — symbol not found / panic.

- [ ] **Step 3: Write `password.rs`**

`crates/roy-auth/src/password.rs`:
```rust
//! bcrypt wrapper used by user-create and login. Cost = bcrypt::DEFAULT_COST.
//! `DUMMY_HASH` is computed once at module-init and used by login to keep
//! response time constant when the username does not exist.

use bcrypt::{hash, verify, DEFAULT_COST, BcryptError};
use once_cell::sync::Lazy;

pub fn hash_password(plain: &str) -> Result<String, BcryptError> {
    hash(plain, DEFAULT_COST)
}

pub fn verify_password(plain: &str, hashed: &str) -> Result<bool, BcryptError> {
    verify(plain, hashed)
}

pub static DUMMY_HASH: Lazy<String> = Lazy::new(|| {
    hash("__roy_dummy_password__", DEFAULT_COST).expect("bcrypt dummy hash")
});
```

Add `once_cell = "1"` to `crates/roy-auth/Cargo.toml` `[dependencies]`.

- [ ] **Step 4: Run all roy-auth tests**

Run: `cargo test -p roy-auth --test store -- --nocapture`
Expected: all four tests PASS (`migration_creates_users_table`, `create_user_then_lookup`, `duplicate_username_rejected`, `short_password_rejected`, `hash_and_verify_round_trip`).

- [ ] **Step 5: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): UserStore + bcrypt password helpers"
```

---

### Task A5: JWT sign/verify

**Files:**
- Modify: `crates/roy-auth/src/jwt.rs`
- Create: `crates/roy-auth/tests/jwt.rs`

- [ ] **Step 1: Write the failing test**

`crates/roy-auth/tests/jwt.rs`:
```rust
use roy_auth::{sign_session, verify_session, JwtError};

const TEST_SECRET: &str = "test-secret-at-least-32-chars-long!!";

#[test]
fn sign_then_verify_roundtrips() {
    let token = sign_session("user-123", TEST_SECRET, 3600).unwrap();
    let sub = verify_session(&token, TEST_SECRET).unwrap();
    assert_eq!(sub, "user-123");
}

#[test]
fn wrong_secret_fails() {
    let token = sign_session("user-123", TEST_SECRET, 3600).unwrap();
    let err = verify_session(&token, "different-secret-32-chars-long!!!!").unwrap_err();
    assert!(matches!(err, JwtError::Invalid));
}

#[test]
fn tampered_payload_fails() {
    let token = sign_session("user-123", TEST_SECRET, 3600).unwrap();
    // Flip a char in the payload segment.
    let mut parts: Vec<&str> = token.split('.').collect();
    let mut payload = parts[1].to_string();
    let last = payload.pop().unwrap();
    payload.push(if last == 'A' { 'B' } else { 'A' });
    parts[1] = &payload;
    let tampered = parts.join(".");
    assert!(matches!(verify_session(&tampered, TEST_SECRET), Err(JwtError::Invalid)));
}

#[test]
fn expired_token_fails() {
    // ttl = -1 ⇒ exp already in the past.
    let token = sign_session("user-123", TEST_SECRET, -1).unwrap();
    assert!(matches!(verify_session(&token, TEST_SECRET), Err(JwtError::Expired)));
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p roy-auth --test jwt -- --nocapture`
Expected: FAIL on missing symbols.

- [ ] **Step 3: Write `jwt.rs`**

`crates/roy-auth/src/jwt.rs`:
```rust
//! HS256 JWT helpers. Payload is `{ sub, iat, exp }` — no extra claims to keep
//! the token small and avoid stale display-name data baked into the token.

use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, errors::ErrorKind, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("invalid token")]
    Invalid,
    #[error("token expired")]
    Expired,
    #[error("secret missing or too short")]
    Secret,
    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    iat: i64,
    exp: i64,
}

pub fn sign_session(user_id: &str, secret: &str, ttl_secs: i64) -> Result<String, JwtError> {
    if secret.len() < 32 {
        return Err(JwtError::Secret);
    }
    let now = Utc::now();
    let claims = Claims {
        sub: user_id.into(),
        iat: now.timestamp(),
        exp: (now + Duration::seconds(ttl_secs)).timestamp(),
    };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret.as_bytes()))
        .map_err(|e| JwtError::Internal(e.to_string()))
}

pub fn verify_session(token: &str, secret: &str) -> Result<String, JwtError> {
    if secret.len() < 32 {
        return Err(JwtError::Secret);
    }
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::new(jsonwebtoken::Algorithm::HS256),
    )
    .map_err(|e| match e.kind() {
        ErrorKind::ExpiredSignature => JwtError::Expired,
        _ => JwtError::Invalid,
    })?;
    Ok(data.claims.sub)
}

/// Read `ROY_JWT_SECRET` from env. Returns `JwtError::Secret` if missing or shorter than 32 bytes.
pub fn secret_from_env() -> Result<String, JwtError> {
    let s = std::env::var("ROY_JWT_SECRET").map_err(|_| JwtError::Secret)?;
    if s.len() < 32 {
        return Err(JwtError::Secret);
    }
    Ok(s)
}
```

- [ ] **Step 4: Run JWT tests**

Run: `cargo test -p roy-auth --test jwt -- --nocapture`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): HS256 JWT sign/verify with env-secret loader"
```

---

### Task A6: `test_support` module

**Files:**
- Modify: `crates/roy-auth/src/test_support.rs`

- [ ] **Step 1: Write `test_support.rs`**

```rust
//! Shared test helpers — gated behind the `test-support` feature so they can be
//! consumed by sibling crates without leaking into release builds.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::types::{NewUser, User};
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
    crate::db::apply_migrations(&pool).await.expect("migrations");
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
```

- [ ] **Step 2: Verify it compiles under the `test-support` feature**

Run: `cargo build -p roy-auth --features test-support`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): test_support module (temp_pool, make_user, issue_jwt)"
```

---

### Task A7: Migration 0011 + `Team` types + `TeamStore`

**Files:**
- Create: `crates/roy-auth/migrations/sqlite/0011_teams.sql`
- Modify: `crates/roy-auth/src/types.rs`
- Modify: `crates/roy-auth/src/team_store.rs`

- [ ] **Step 1: Write migration**

`crates/roy-auth/migrations/sqlite/0011_teams.sql`:
```sql
CREATE TABLE teams (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    created_by  TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE team_members (
    user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    team_id   TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    role      TEXT NOT NULL DEFAULT 'member',
    joined_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, team_id)
);

CREATE INDEX team_members_by_team ON team_members(team_id);
```

- [ ] **Step 2: Replace `Team*` placeholders in `types.rs`**

Replace the `Team`, `NewTeam`, `TeamMember`, `TeamMembership` stub structs with:
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewTeam {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct TeamMember {
    pub user_id: String,
    pub team_id: String,
    pub role: String,                                  // "owner" | "member"
    pub joined_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamMembership {
    pub id: String,
    pub name: String,
    pub role: Role,
}
```

- [ ] **Step 3: Write `TeamStore`**

`crates/roy-auth/src/team_store.rs`:
```rust
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::types::{NewTeam, Role, Team, TeamMembership};

#[derive(Debug, thiserror::Error)]
pub enum TeamStoreError {
    #[error("team not found: {0}")]
    NotFound(String),
    #[error("forbidden")]
    Forbidden,
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct TeamStore { pool: SqlitePool }

impl TeamStore {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    /// Create a team with `created_by` as the owner. Inserts both rows in one tx.
    pub async fn create(&self, new: NewTeam, owner_id: &str) -> Result<Team, TeamStoreError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp_millis();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO teams (id, name, description, created_by, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id).bind(&new.name).bind(&new.description).bind(owner_id).bind(now)
        .execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO team_members (user_id, team_id, role, joined_at)
             VALUES (?, ?, 'owner', ?)",
        )
        .bind(owner_id).bind(&id).bind(now)
        .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Team { id, name: new.name, description: new.description, created_by: Some(owner_id.into()), created_at: now })
    }

    pub async fn list_for_user(&self, user_id: &str) -> Result<Vec<TeamMembership>, TeamStoreError> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT teams.id, teams.name, team_members.role
             FROM teams INNER JOIN team_members ON team_members.team_id = teams.id
             WHERE team_members.user_id = ?
             ORDER BY teams.created_at",
        )
        .bind(user_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id, name, role)| TeamMembership {
            id, name,
            role: if role == "owner" { Role::Owner } else { Role::Member },
        }).collect())
    }

    pub async fn is_member(&self, user_id: &str, team_id: &str) -> Result<bool, TeamStoreError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM team_members WHERE user_id = ? AND team_id = ?",
        )
        .bind(user_id).bind(team_id).fetch_optional(&self.pool).await?;
        Ok(row.is_some())
    }

    pub async fn is_owner(&self, user_id: &str, team_id: &str) -> Result<bool, TeamStoreError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT role FROM team_members WHERE user_id = ? AND team_id = ?",
        )
        .bind(user_id).bind(team_id).fetch_optional(&self.pool).await?;
        Ok(matches!(row, Some((r,)) if r == "owner"))
    }

    pub async fn delete(&self, team_id: &str) -> Result<(), TeamStoreError> {
        let res = sqlx::query("DELETE FROM teams WHERE id = ?")
            .bind(team_id).execute(&self.pool).await?;
        if res.rows_affected() == 0 {
            return Err(TeamStoreError::NotFound(team_id.into()));
        }
        Ok(())
    }

    pub async fn add_member(&self, team_id: &str, user_id: &str) -> Result<(), TeamStoreError> {
        let now = Utc::now().timestamp_millis();
        let res = sqlx::query(
            "INSERT OR IGNORE INTO team_members (user_id, team_id, role, joined_at)
             VALUES (?, ?, 'member', ?)",
        )
        .bind(user_id).bind(team_id).bind(now)
        .execute(&self.pool).await?;
        if res.rows_affected() == 0 {
            // already member — idempotent
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Add `test_support::make_team` helper**

Append to `crates/roy-auth/src/test_support.rs`:
```rust
use crate::team_store::TeamStore;
use crate::types::{NewTeam, Team};

pub async fn make_team(pool: &SqlitePool, owner_id: &str, name: &str) -> Team {
    TeamStore::new(pool.clone())
        .create(NewTeam { name: name.into(), description: None }, owner_id)
        .await
        .expect("make_team")
}
```

- [ ] **Step 5: Write team tests**

Append to `crates/roy-auth/tests/store.rs`:
```rust
use roy_auth::{NewTeam, TeamStore};

#[tokio::test]
async fn create_team_lists_owner() {
    let pool = fresh_pool().await;
    let users = UserStore::new(pool.clone());
    let alice = users.create(NewUser { username: "alice".into(), display_name: "A".into(), password: "12345678".into(), timezone: None }).await.unwrap();
    let teams = TeamStore::new(pool.clone());
    let team = teams.create(NewTeam { name: "eng".into(), description: None }, &alice.id).await.unwrap();

    let memberships = teams.list_for_user(&alice.id).await.unwrap();
    assert_eq!(memberships.len(), 1);
    assert_eq!(memberships[0].id, team.id);
    assert_eq!(memberships[0].name, "eng");
    assert!(teams.is_owner(&alice.id, &team.id).await.unwrap());
    assert!(teams.is_member(&alice.id, &team.id).await.unwrap());

    // Bob is not a member.
    let bob = users.create(NewUser { username: "bob".into(), display_name: "B".into(), password: "12345678".into(), timezone: None }).await.unwrap();
    assert!(!teams.is_member(&bob.id, &team.id).await.unwrap());
}
```

Add `use roy_auth::{NewUser, UserStore};` if not already present.

- [ ] **Step 6: Run tests**

Run: `cargo test -p roy-auth --test store create_team_lists_owner`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): teams migration (0011) + TeamStore"
```

---

### Task A8: Migration 0012 + `InviteStore`

**Files:**
- Create: `crates/roy-auth/migrations/sqlite/0012_team_invites.sql`
- Modify: `crates/roy-auth/src/types.rs`
- Modify: `crates/roy-auth/src/invite_store.rs`
- Create: `crates/roy-auth/tests/invites.rs`

- [ ] **Step 1: Write migration**

`crates/roy-auth/migrations/sqlite/0012_team_invites.sql`:
```sql
CREATE TABLE team_invites (
    token        TEXT PRIMARY KEY,
    team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    created_by   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    accepted_by  TEXT REFERENCES users(id) ON DELETE SET NULL,
    accepted_at  INTEGER
);
```

- [ ] **Step 2: Replace `TeamInvite` placeholder**

In `types.rs`:
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct TeamInvite {
    pub token: String,
    pub team_id: String,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub accepted_by: Option<String>,
    pub accepted_at: Option<i64>,
}
```

- [ ] **Step 3: Write `InviteStore`**

`crates/roy-auth/src/invite_store.rs`:
```rust
use chrono::Utc;
use rand::RngCore;
use sqlx::SqlitePool;

use crate::team_store::TeamStore;
use crate::types::TeamInvite;

#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("invite invalid")]
    Invalid,
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct InviteStore { pool: SqlitePool }

impl InviteStore {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn create(&self, team_id: &str, created_by: &str, expires_at: Option<i64>) -> Result<TeamInvite, InviteError> {
        let mut buf = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut buf);
        let token = hex::encode(buf);
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO team_invites (token, team_id, created_by, created_at, expires_at) VALUES (?,?,?,?,?)",
        )
        .bind(&token).bind(team_id).bind(created_by).bind(now).bind(expires_at)
        .execute(&self.pool).await?;
        Ok(TeamInvite { token, team_id: team_id.into(), created_by: created_by.into(), created_at: now, expires_at, accepted_by: None, accepted_at: None })
    }

    /// Accept an invite for `user_id`. All failure modes collapse to `Invalid`
    /// (anti-enumeration). On success: adds the user to the team, marks the
    /// invite consumed, returns the team_id.
    pub async fn accept(&self, token: &str, user_id: &str) -> Result<String, InviteError> {
        let mut tx = self.pool.begin().await?;
        let row: Option<TeamInvite> = sqlx::query_as("SELECT * FROM team_invites WHERE token = ?")
            .bind(token).fetch_optional(&mut *tx).await?;
        let invite = row.ok_or(InviteError::Invalid)?;
        if invite.accepted_by.is_some() { return Err(InviteError::Invalid); }
        if let Some(exp) = invite.expires_at {
            if Utc::now().timestamp_millis() > exp { return Err(InviteError::Invalid); }
        }
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE team_invites SET accepted_by = ?, accepted_at = ? WHERE token = ?",
        )
        .bind(user_id).bind(now).bind(token).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT OR IGNORE INTO team_members (user_id, team_id, role, joined_at) VALUES (?,?,'member',?)",
        )
        .bind(user_id).bind(&invite.team_id).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        let _ = TeamStore::new(self.pool.clone());        // keep dep visible
        Ok(invite.team_id)
    }
}
```

- [ ] **Step 4: Write tests**

`crates/roy-auth/tests/invites.rs`:
```rust
use roy_auth::{InviteError, InviteStore, NewTeam, NewUser, TeamStore, UserStore};
use sqlx::SqlitePool;

async fn fresh_pool() -> SqlitePool {
    let pool = roy_auth::test_support::temp_pool().await;
    pool
}

#[tokio::test]
async fn accept_invite_adds_member() {
    let pool = fresh_pool().await;
    let alice = UserStore::new(pool.clone()).create(NewUser { username: "alice".into(), display_name: "A".into(), password: "12345678".into(), timezone: None }).await.unwrap();
    let bob = UserStore::new(pool.clone()).create(NewUser { username: "bob".into(), display_name: "B".into(), password: "12345678".into(), timezone: None }).await.unwrap();
    let teams = TeamStore::new(pool.clone());
    let team = teams.create(NewTeam { name: "eng".into(), description: None }, &alice.id).await.unwrap();

    let invites = InviteStore::new(pool.clone());
    let inv = invites.create(&team.id, &alice.id, None).await.unwrap();

    let tid = invites.accept(&inv.token, &bob.id).await.unwrap();
    assert_eq!(tid, team.id);
    assert!(teams.is_member(&bob.id, &team.id).await.unwrap());
}

#[tokio::test]
async fn consumed_invite_rejected() {
    let pool = fresh_pool().await;
    let alice = UserStore::new(pool.clone()).create(NewUser { username: "alice".into(), display_name: "A".into(), password: "12345678".into(), timezone: None }).await.unwrap();
    let team = TeamStore::new(pool.clone()).create(NewTeam { name: "eng".into(), description: None }, &alice.id).await.unwrap();
    let invites = InviteStore::new(pool.clone());
    let inv = invites.create(&team.id, &alice.id, None).await.unwrap();
    invites.accept(&inv.token, &alice.id).await.unwrap();
    assert!(matches!(invites.accept(&inv.token, &alice.id).await, Err(InviteError::Invalid)));
}

#[tokio::test]
async fn expired_invite_rejected() {
    let pool = fresh_pool().await;
    let alice = UserStore::new(pool.clone()).create(NewUser { username: "alice".into(), display_name: "A".into(), password: "12345678".into(), timezone: None }).await.unwrap();
    let team = TeamStore::new(pool.clone()).create(NewTeam { name: "eng".into(), description: None }, &alice.id).await.unwrap();
    let invites = InviteStore::new(pool.clone());
    let inv = invites.create(&team.id, &alice.id, Some(0)).await.unwrap();  // already expired
    assert!(matches!(invites.accept(&inv.token, &alice.id).await, Err(InviteError::Invalid)));
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p roy-auth --test invites -- --nocapture`
Expected: 3 PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): team_invites migration (0012) + InviteStore"
```

---

### Task A9: `Acl` helper

**Files:**
- Modify: `crates/roy-auth/src/acl.rs`

- [ ] **Step 1: Write `acl.rs`**

```rust
//! Permission checks expressed as guard methods. Each method returns Ok(()) on
//! success and AclError::Forbidden otherwise. Callers run them before any FS
//! or DB write.

use sqlx::SqlitePool;

use crate::team_store::TeamStore;
use crate::types::Scope;

#[derive(Debug, thiserror::Error)]
pub enum AclError {
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

pub struct Acl<'a> {
    pub pool: &'a SqlitePool,
    pub user_id: &'a str,
}

impl<'a> Acl<'a> {
    pub fn new(pool: &'a SqlitePool, user_id: &'a str) -> Self { Self { pool, user_id } }

    pub async fn can_access_scope(&self, scope: &Scope) -> Result<(), AclError> {
        match scope {
            Scope::Personal => Ok(()),
            Scope::Team { team_id } => {
                let ok = TeamStore::new(self.pool.clone()).is_member(self.user_id, team_id).await.map_err(|_| AclError::Forbidden)?;
                if ok { Ok(()) } else { Err(AclError::Forbidden) }
            }
        }
    }

    pub async fn can_admin_team(&self, team_id: &str) -> Result<(), AclError> {
        let ok = TeamStore::new(self.pool.clone()).is_owner(self.user_id, team_id).await.map_err(|_| AclError::Forbidden)?;
        if ok { Ok(()) } else { Err(AclError::Forbidden) }
    }

    /// Project belongs to the user (created_by) or to a team they're a member of.
    pub async fn can_access_project(&self, project_id: &str) -> Result<(), AclError> {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT created_by, team_id FROM projects WHERE id = ?",
        )
        .bind(project_id).fetch_optional(self.pool).await?;
        let (created_by, team_id) = row.ok_or(AclError::NotFound)?;
        if let Some(team_id) = team_id {
            self.can_access_scope(&Scope::Team { team_id }).await
        } else if created_by == self.user_id {
            Ok(())
        } else {
            Err(AclError::Forbidden)
        }
    }
}
```

- [ ] **Step 2: Compile-only check (no tests yet — `projects` doesn't have `created_by`/`team_id` until Phase B)**

Run: `cargo build -p roy-auth`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): Acl guard methods for scope/team/project"
```

---

### Task A10: `cookie.rs` — verify_cookie + verify_ws_protocol

**Files:**
- Modify: `crates/roy-auth/src/cookie.rs`

- [ ] **Step 1: Write `cookie.rs`**

```rust
//! HTTP `Cookie:` parser + WS `Sec-WebSocket-Protocol` parser. Both reduce to
//! `verify_session` against `ROY_JWT_SECRET`.

use crate::jwt::{secret_from_env, verify_session, JwtError};

pub const COOKIE_NAME: &str = "roy-jwt";

/// Extract `roy-jwt=...` from a raw Cookie header value. Returns None if not present.
pub fn read_jwt_cookie(header_value: &str) -> Option<&str> {
    for part in header_value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
            return Some(rest);
        }
    }
    None
}

/// Verify a Cookie-header value. Returns the user id on success.
pub fn verify_cookie(header_value: &str) -> Result<String, JwtError> {
    let token = read_jwt_cookie(header_value).ok_or(JwtError::Invalid)?;
    let secret = secret_from_env()?;
    verify_session(token, &secret)
}

/// Verify a `Sec-WebSocket-Protocol` header. Browsers can't set custom headers
/// during WS handshake, so the JWT travels as a subprotocol value alongside the
/// literal `roy-jwt` marker — same convention as the existing shared-token flow
/// in ws.rs.
pub fn verify_ws_protocol(header_value: &str) -> Result<String, JwtError> {
    // header looks like "roy-jwt,<JWT>" or "roy-jwt, <JWT>"
    let mut parts = header_value.split(',').map(str::trim);
    let marker = parts.next().unwrap_or("");
    if marker != "roy-jwt" { return Err(JwtError::Invalid); }
    let token = parts.next().ok_or(JwtError::Invalid)?;
    let secret = secret_from_env()?;
    verify_session(token, &secret)
}
```

- [ ] **Step 2: Write tests in `tests/jwt.rs`**

Append:
```rust
#[test]
fn cookie_parser_extracts_token() {
    let raw = "other=1; roy-jwt=abc.def.ghi; foo=bar";
    assert_eq!(roy_auth::cookie::read_jwt_cookie(raw), Some("abc.def.ghi"));
}

#[test]
fn cookie_parser_returns_none_when_missing() {
    assert_eq!(roy_auth::cookie::read_jwt_cookie("foo=bar"), None);
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p roy-auth --test jwt -- --nocapture`
Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-auth
git commit -m "feat(roy-auth): cookie/WS-subprotocol verification entrypoints"
```

---

### **Checkpoint A** — `roy-auth` foundation complete

Run: `cargo test --workspace --no-fail-fast`
Expected: all existing tests still green; `roy-auth` adds passing JWT, store, invites suites.

---

## Phase B — `roy-management` auth wiring

### Task B1: Add `roy-auth` dep + extend `AppState`

**Files:**
- Modify: `crates/roy-management/Cargo.toml`
- Modify: `crates/roy-management/src/state.rs`

- [ ] **Step 1: Add dependencies to `crates/roy-management/Cargo.toml`**

In `[dependencies]`:
```toml
roy-auth = { path = "../roy-auth" }
axum-extra = { version = "0.10", features = ["cookie"] }
```

In `[dev-dependencies]`:
```toml
roy-auth = { path = "../roy-auth", features = ["test-support"] }
```

- [ ] **Step 2: Extend `AppState`**

`crates/roy-management/src/state.rs`:
```rust
use std::path::PathBuf;
use std::sync::Arc;

use roy_agents::Store;
use sqlx::SqlitePool;

use crate::meta_store::MetaStore;
use crate::roy_client::DaemonClient;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub meta: MetaStore,
    pub daemon: Arc<dyn DaemonClient>,
    pub socket_path: PathBuf,
    pub scheduler_pool: Option<SqlitePool>,
    /// Shared sqlite pool — needed by roy-auth middleware/handlers and ACL.
    pub pool: SqlitePool,
    /// Workspace root for resolve_cwd.
    pub workspace_dir: PathBuf,
}
```

- [ ] **Step 3: Update construction in `lib.rs`**

In `crates/roy-management/src/lib.rs::run`, replace the `AppState { ... }` block with:
```rust
let state = AppState {
    store: roy_agents::Store::new(pool.clone()),
    meta,
    daemon,
    socket_path: socket,
    scheduler_pool,
    pool: pool.clone(),
    workspace_dir: workspace_dir.clone(),
};
```

(`workspace_dir` is already created above.)

- [ ] **Step 4: Run migrations early in `run()`**

Insert after `MetaStore::apply_migrations(&pool).await?;`:
```rust
roy_auth::apply_migrations(&pool).await?;
```

- [ ] **Step 5: Compile**

Run: `cargo build -p roy-management`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management
git commit -m "chore(roy-management): wire roy-auth pool + apply_migrations"
```

---

### Task B2: Migration 0005 — owners on projects + session_meta

**Files:**
- Create: `crates/roy-management/migrations/sqlite/0005_owners.sql`
- Modify: `crates/roy-management/src/meta_store.rs`

- [ ] **Step 1: Write the migration**

```sql
DELETE FROM session_tags;
DELETE FROM session_meta;
DELETE FROM projects;

DROP TABLE projects;
CREATE TABLE projects (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    path       TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users(id),
    team_id    TEXT REFERENCES teams(id),
    created_at INTEGER NOT NULL
);

DROP TABLE session_meta;
CREATE TABLE session_meta (
    session_id    TEXT PRIMARY KEY,
    project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
    agent_id      TEXT,
    agent_name    TEXT,
    display_label TEXT,
    created_by    TEXT NOT NULL REFERENCES users(id),
    team_id       TEXT REFERENCES teams(id),
    created_at    INTEGER NOT NULL
);

CREATE TABLE session_tags (
    session_id TEXT NOT NULL REFERENCES session_meta(session_id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    PRIMARY KEY (session_id, key)
);
```

- [ ] **Step 2: Extend `Project`/`SessionMeta` Rust structs in `meta_store.rs`**

Replace:
```rust
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_by: String,
    pub team_id: Option<String>,
    pub created_at: i64,
}

pub struct SessionMeta {
    pub session_id: String,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub display_label: Option<String>,
    pub created_by: String,
    pub team_id: Option<String>,
    pub tags: BTreeMap<String, String>,
    pub created_at: i64,
}
```

- [ ] **Step 3: Update all INSERT/SELECT/UPDATE statements in `meta_store.rs` to include the new columns.**

`create_project` becomes `create_project_v2(name, path, created_by, team_id) -> Project`. The old signature is rewritten in place — there are no other callers because Phase B fully replaces session/project handlers.

Look at `crates/roy-management/src/meta_store.rs` around lines 82-109 (`create_project`), 111-125 (`list_projects`), 168-200 (`upsert_session_meta`). Update each query to include the new columns. Examples:

```rust
pub async fn create_project(
    &self,
    name: &str,
    created_by: &str,
    team_id: Option<&str>,
) -> Result<Project, MetaError> {
    validate_project_name(name)?;
    let id = uuid::Uuid::new_v4().to_string();
    let dir = self.workspace_dir.join(name);
    std::fs::create_dir_all(&dir)?;
    let path = dir.to_string_lossy().into_owned();
    let created_at = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO projects (id, name, path, created_by, team_id, created_at) VALUES (?,?,?,?,?,?)",
    )
    .bind(&id).bind(name).bind(&path).bind(created_by).bind(team_id).bind(created_at)
    .execute(&self.pool).await?;
    Ok(Project { id, name: name.into(), path, created_by: created_by.into(), team_id: team_id.map(|s| s.into()), created_at })
}

pub async fn list_projects_for_user(
    &self,
    user_id: &str,
    team_ids: &[String],
) -> Result<Vec<Project>, MetaError> {
    // Personal (team_id IS NULL AND created_by = ?) ∪ projects belonging to teams the user is in
    let mut q = String::from(
        "SELECT id, name, path, created_by, team_id, created_at FROM projects \
         WHERE (team_id IS NULL AND created_by = ?)",
    );
    if !team_ids.is_empty() {
        q.push_str(" OR team_id IN (");
        for (i, _) in team_ids.iter().enumerate() {
            if i > 0 { q.push(','); }
            q.push('?');
        }
        q.push(')');
    }
    q.push_str(" ORDER BY created_at");
    let mut query = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(&q).bind(user_id);
    for tid in team_ids { query = query.bind(tid); }
    let rows = query.fetch_all(&self.pool).await?;
    Ok(rows.into_iter().map(|r| Project {
        id: r.0, name: r.1, path: r.2, created_by: r.3, team_id: r.4, created_at: r.5
    }).collect())
}
```

Update `upsert_session_meta` to accept the new columns. Update its SQL to:
```sql
INSERT INTO session_meta (session_id, project_id, agent_id, agent_name, display_label, created_by, team_id, created_at)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(session_id) DO UPDATE SET
    project_id    = excluded.project_id,
    agent_id      = excluded.agent_id,
    agent_name    = excluded.agent_name,
    display_label = excluded.display_label
```
(We don't change `created_by`/`team_id` on conflict.)

Update its read paths (`get_session_meta`, `list_session_meta`) to project the new columns.

- [ ] **Step 4: Update existing in-crate callers**

The HTTP layer (`http.rs::create_project`, `create_session`, etc.) currently calls `meta.create_project(name)` without user_id. We will fix that in Task B5. For now, **all references to the old signatures must compile**. To keep compile-time green during transition, keep the **new** signatures and update the few call sites in `http.rs` and tests with a stub `"root"` as `created_by` until B5 lands. (B5 replaces them with extractor-based user_id.)

- [ ] **Step 5: Run all tests**

Run: `cargo test -p roy-management --no-fail-fast`
Expected: existing tests still PASS (with stubbed `"root"` user). Migration applies cleanly because we drop pre-existing rows.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): owners migration (0005) + project/session_meta with created_by + team_id"
```

---

### Task B3: Bootstrap-root in `lib.rs::run()`

**Files:**
- Create: `crates/roy-management/src/bootstrap.rs`
- Modify: `crates/roy-management/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/roy-management/tests/auth_flow.rs` (create file):
```rust
use roy_auth::test_support::temp_pool;
use roy_management::bootstrap::ensure_root;

#[tokio::test]
async fn bootstrap_creates_user_when_table_empty() {
    let pool = temp_pool().await;
    std::env::set_var("ROY_BOOTSTRAP_PASSWORD", "bootstrap-test-pw-1");
    let created = ensure_root(&pool).await.unwrap();
    assert!(created);  // first call inserts

    let again = ensure_root(&pool).await.unwrap();
    assert!(!again);   // second call is no-op

    let user = roy_auth::UserStore::new(pool.clone()).get_by_username("root").await.unwrap();
    assert_eq!(user.username, "root");
}
```

- [ ] **Step 2: Run test (fails on missing module)**

Run: `cargo test -p roy-management --test auth_flow bootstrap_creates_user_when_table_empty`
Expected: FAIL.

- [ ] **Step 3: Write `bootstrap.rs`**

```rust
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
    store.create(NewUser { username, display_name, password, timezone: None }).await?;
    Ok(true)
}
```

Add to `crates/roy-management/src/lib.rs`:
```rust
pub mod bootstrap;
```

In `run()` after `roy_auth::apply_migrations(...)`:
```rust
bootstrap::ensure_root(&pool).await?;
```

- [ ] **Step 4: Add deps**

To `crates/roy-management/Cargo.toml`:
```toml
rand = "0.8"
hex = "0.4"
```

- [ ] **Step 5: Run test**

Run: `cargo test -p roy-management --test auth_flow bootstrap_creates_user_when_table_empty -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): bootstrap-root on first start"
```

---

### Task B4: Auth handlers + `require_user` middleware

**Files:**
- Create: `crates/roy-management/src/auth.rs`
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/src/lib.rs`

- [ ] **Step 1: Write failing integration test**

Append to `crates/roy-management/tests/auth_flow.rs`:
```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use roy_auth::test_support::TEST_JWT_SECRET;
use roy_management::http::router_for_tests;
use roy_management::state::AppState;
use tower::ServiceExt;

async fn test_app() -> (axum::Router, sqlx::SqlitePool) {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let pool = roy_auth::test_support::temp_pool().await;
    roy_management::meta_store::MetaStore::apply_migrations(&pool).await.unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    std::mem::forget(workspace_dir);
    let dir = std::env::temp_dir().join(format!("roy-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let meta = roy_management::meta_store::MetaStore::new(pool.clone(), dir.clone());

    let daemon: std::sync::Arc<dyn roy_management::roy_client::DaemonClient> = std::sync::Arc::new(
        roy_management::roy_client::mock::MockDaemonClient::new().with_spawn("sess-1"),
    );
    let state = AppState {
        store: roy_agents::Store::new(pool.clone()),
        meta,
        daemon,
        socket_path: std::path::PathBuf::from("/tmp/fake.sock"),
        scheduler_pool: None,
        pool: pool.clone(),
        workspace_dir: dir,
    };
    (router_for_tests(state), pool)
}

#[tokio::test]
async fn login_sets_cookie_then_me_returns_profile() {
    let (app, pool) = test_app().await;
    let alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let _ = alice;

    let body = serde_json::to_vec(&serde_json::json!({"username":"alice","password":"test-password-1234"})).unwrap();
    let resp = app.clone().oneshot(
        Request::post("/auth/login")
            .header("content-type","application/json")
            .body(Body::from(body)).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    assert!(cookie.starts_with("roy-jwt="));

    let me = app.oneshot(
        Request::get("/auth/me").header("cookie", &cookie).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    let bytes = me.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["username"], "alice");
}

#[tokio::test]
async fn me_without_cookie_is_unauthorized() {
    let (app, _pool) = test_app().await;
    let resp = app.oneshot(Request::get("/auth/me").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_wrong_password_is_401() {
    let (app, pool) = test_app().await;
    let _ = roy_auth::test_support::make_user(&pool, "alice").await;
    let body = serde_json::to_vec(&serde_json::json!({"username":"alice","password":"WRONG-PASSWORD"})).unwrap();
    let resp = app.oneshot(
        Request::post("/auth/login").header("content-type","application/json").body(Body::from(body)).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run test (fails on missing handlers + missing FakeDaemon::test_support)**

Run: `cargo test -p roy-management --test auth_flow -- --nocapture`
Expected: FAIL with module-not-found / unresolved-symbol.

- [ ] **Step 3: Write `auth.rs`**

`crates/roy-management/src/auth.rs`:
```rust
//! HTTP-side authentication: login/logout/me handlers, axum middleware that
//! resolves the JWT cookie into a `user_id`, and an `AuthUser` extension type
//! handlers consume via `Extension<AuthUser>`.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use roy_auth::{
    cookie::{verify_cookie, COOKIE_NAME},
    jwt::{secret_from_env, sign_session},
    password::{verify_password, DUMMY_HASH},
    team_store::TeamStore,
    user_store::UserStore,
    types::UserProfile,
};
use serde::Deserialize;

use crate::state::AppState;

#[derive(Clone, Debug)]
pub struct AuthUser(pub String);

#[derive(Deserialize)]
struct LoginReq { username: String, password: String }

const COOKIE_MAX_AGE: i64 = 60 * 60 * 24 * 7;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
}

pub async fn require_user(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let cookie_header = req.headers().get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match verify_cookie(cookie_header) {
        Ok(user_id) => {
            req.extensions_mut().insert(AuthUser(user_id));
            next.run(req).await
        }
        Err(_) => (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"auth required"}))).into_response(),
    }
}

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> Response {
    let secret = match secret_from_env() {
        Ok(s) => s,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":"server misconfigured"}))).into_response(),
    };
    let row = UserStore::new(state.pool.clone()).get_by_username(&req.username).await.ok();
    let hash = row.as_ref().map(|u| u.password_hash.as_str()).unwrap_or(&DUMMY_HASH);
    let ok = verify_password(&req.password, hash).unwrap_or(false);
    if !ok || row.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"invalid credentials"}))).into_response();
    }
    let user = row.unwrap();
    let token = match sign_session(&user.id, &secret, COOKIE_MAX_AGE) {
        Ok(t) => t,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":"internal error"}))).into_response(),
    };
    let secure = std::env::var("ROY_HTTPS").ok().as_deref() == Some("1");
    let cookie = format!(
        "{}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={COOKIE_MAX_AGE}{}",
        COOKIE_NAME,
        if secure { "; Secure" } else { "" },
    );
    let mut resp = Json(profile_for(&state, &user.id).await).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

async fn logout() -> Response {
    let cookie = format!("{}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0", COOKIE_NAME);
    let mut resp = (StatusCode::NO_CONTENT).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

async fn me(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(state): State<AppState>,
) -> Response {
    Json(profile_for(&state, &uid).await).into_response()
}

async fn profile_for(state: &AppState, user_id: &str) -> UserProfile {
    let user = UserStore::new(state.pool.clone()).get(user_id).await.expect("user gone");
    let teams = TeamStore::new(state.pool.clone()).list_for_user(user_id).await.unwrap_or_default();
    UserProfile {
        id: user.id,
        username: user.username,
        display_name: user.display_name,
        timezone: user.timezone,
        teams,
    }
}
```

- [ ] **Step 4: Mount in `http.rs`**

In `crates/roy-management/src/http.rs::router`, replace the `Router::new()` chain with:

```rust
use crate::auth;

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/_builder", post(start_builder))
        .route("/agents/{id}", get(get_agent).put(update_agent).delete(delete_agent))
        .route("/agents/{id}/run", post(run_agent))
        .route("/presets", get(list_presets))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", axum::routing::delete(delete_project).put(update_project))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session).patch(patch_session))
        .route("/sessions/{id}/tags", axum::routing::put(put_tags))
        .route("/scheduler/agents", get(list_scheduler_agents))
        .route("/scheduler/triggers", get(list_scheduler_triggers))
        .route("/scheduler/fires", get(list_scheduler_fires))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_user));

    auth::router()
        .merge(protected)
        .with_state(state)
}

/// Same as `router` but exposes everything for tests including unauth routes.
pub fn router_for_tests(state: AppState) -> Router {
    router(state)
}
```

- [ ] **Step 5: Run tests**

Run: `ROY_JWT_SECRET=$(printf 'roy-test-jwt-secret-32-chars-min!!') cargo test -p roy-management --test auth_flow -- --nocapture`
Expected: all 4 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): /auth/login /auth/logout /auth/me + require_user middleware"
```

---

### Task B5: Wire `AuthUser` into existing handlers

**Files:**
- Modify: `crates/roy-management/src/http.rs`

The existing `create_project`/`create_session`/`patch_session` handlers ignore the user. Now they must pull `AuthUser` and pass `user_id` (and optional `team_id`) to MetaStore.

For each handler at the call sites listed below, **add the extractor and pass values through**:

`create_project` signature:
```rust
async fn create_project(
    axum::extract::Extension(AuthUser(user_id)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> Result<Json<Project>, ApiError> {
    if let Some(team_id) = &req.team_id {
        roy_auth::Acl::new(&s.pool, &user_id).can_admin_team(team_id).await.map_err(|_| ApiError(StatusCode::FORBIDDEN, "forbidden".into()))?;
    }
    s.meta.create_project(&req.name, &user_id, req.team_id.as_deref()).await
        .map(Json).map_err(map_meta_err)
}
```

Define `CreateProjectReq` near the handler:
```rust
#[derive(serde::Deserialize)]
struct CreateProjectReq {
    name: String,
    #[serde(default)]
    team_id: Option<String>,
}
```

`list_projects` now filters by user/teams:
```rust
async fn list_projects(
    axum::extract::Extension(AuthUser(user_id)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
) -> Result<Json<Vec<Project>>, ApiError> {
    let teams = roy_auth::TeamStore::new(s.pool.clone()).list_for_user(&user_id).await
        .map_err(|e| { tracing::warn!(error=%e, "team list"); ApiError(StatusCode::INTERNAL_SERVER_ERROR,"internal".into()) })?;
    let team_ids: Vec<String> = teams.into_iter().map(|t| t.id).collect();
    s.meta.list_projects_for_user(&user_id, &team_ids).await.map(Json).map_err(map_meta_err)
}
```

`create_session` is handled in Task C2. For now, **stub it** to require AuthUser but keep the legacy behavior (cwd = workspace_dir):
```rust
async fn create_session(
    axum::extract::Extension(AuthUser(user_id)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    Json(req): Json<CreateSessionReq>,
) -> Result<Json<SessionMeta>, ApiError> {
    // Per-scope cwd lands in Task C2. Until then, sessions are personal,
    // cwd = workspace_dir/users/<uid>/sessions/<sid>.
    ...
}
```

- [ ] **Step 1: Update each handler signature above. Add `roy_auth` import at the top of `http.rs`.**

- [ ] **Step 2: Compile**

Run: `cargo build -p roy-management`
Expected: PASS.

- [ ] **Step 3: Update existing handler tests in `http.rs`'s `#[cfg(test)] mod tests` to set a cookie before calling protected routes.**

Look at lines 800+ in `http.rs` — each `oneshot(Request::...)` needs a `.header("cookie", &cookie)` line and a setup `make_user + sign_session` helper. Use `roy_auth::test_support::{TEST_JWT_SECRET, make_user}` and `roy_auth::jwt::sign_session(&user.id, TEST_JWT_SECRET, 3600)`.

- [ ] **Step 4: Run all tests**

Run: `cargo test -p roy-management --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): require AuthUser on /projects, /agents, /sessions handlers"
```

---

### Task B6: Rate limit on `/auth/login`

**Files:**
- Create: `crates/roy-management/src/rate_limit.rs`
- Modify: `crates/roy-management/src/auth.rs`
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/tests/auth_flow.rs`

- [ ] **Step 1: Write failing test**

Append to `auth_flow.rs`:
```rust
#[tokio::test]
async fn login_rate_limit_blocks_after_5_failures() {
    let (app, _pool) = test_app().await;
    for _ in 0..5 {
        let body = serde_json::to_vec(&serde_json::json!({"username":"nope","password":"nope"})).unwrap();
        let resp = app.clone().oneshot(
            Request::post("/auth/login")
                .header("content-type","application/json")
                .header("x-forwarded-for","1.2.3.4")
                .body(Body::from(body)).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    let body = serde_json::to_vec(&serde_json::json!({"username":"nope","password":"nope"})).unwrap();
    let resp = app.oneshot(
        Request::post("/auth/login")
            .header("content-type","application/json")
            .header("x-forwarded-for","1.2.3.4")
            .body(Body::from(body)).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}
```

The test forces `X-Forwarded-For` since axum's tower test infra doesn't carry a peer IP. Make the rate limiter prefer that header when `ROY_TRUSTED_PROXIES=*` is set (test sets it via `std::env::set_var` in `test_app`).

- [ ] **Step 2: Write `rate_limit.rs`**

```rust
//! In-memory token bucket per IP. 5 attempts / 5 minutes. Wraps the login
//! handler only; no other endpoint pays for it. Resets on process restart.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_ATTEMPTS: u32 = 5;
const WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy)]
struct Bucket { tokens: u32, refilled_at: Instant }

#[derive(Default)]
pub struct LoginLimiter { buckets: Mutex<HashMap<IpAddr, Bucket>> }

impl LoginLimiter {
    pub fn check(&self, ip: IpAddr) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.entry(ip).or_insert(Bucket { tokens: MAX_ATTEMPTS, refilled_at: Instant::now() });
        if bucket.refilled_at.elapsed() >= WINDOW {
            bucket.tokens = MAX_ATTEMPTS;
            bucket.refilled_at = Instant::now();
        }
        if bucket.tokens == 0 { return false; }
        bucket.tokens -= 1;
        true
    }
}
```

- [ ] **Step 3: Wire it into login**

In `auth.rs`:
1. Add `pub fn extract_ip(headers: &axum::http::HeaderMap, trusted_proxies: bool) -> std::net::IpAddr` — reads `X-Forwarded-For` when `trusted_proxies` is true, else returns `127.0.0.1`.
2. Add a `Arc<LoginLimiter>` field to `AppState`.
3. In `login()`, call `state.login_limiter.check(ip)` first; if false → `429`.

```rust
let ip = extract_ip(&headers, std::env::var("ROY_TRUSTED_PROXIES").is_ok());
if !state.login_limiter.check(ip) {
    return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({"error":"too many attempts"}))).into_response();
}
```

(`headers` is extracted via `axum::http::HeaderMap`; add it to the `login` signature.)

- [ ] **Step 4: Run test**

Run: `cargo test -p roy-management --test auth_flow login_rate_limit_blocks_after_5_failures -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): IP rate limit on /auth/login (5/5min)"
```

---

### **Checkpoint B** — `roy-management` auth is working

Run: `cargo test --workspace --no-fail-fast`
Expected: all green. At this point a user can login via curl, get a JWT cookie, hit `/auth/me`, create projects (filtered by ownership). Sessions still use the stub cwd path — fixed in Phase C.

---

## Phase C — Per-scope cwd in sessions/projects

### Task C1: `cwd.rs` — resolve_cwd + path-traversal guard

**Files:**
- Create: `crates/roy-management/src/cwd.rs`
- Modify: `crates/roy-management/src/lib.rs`
- Create: `crates/roy-management/tests/session_cwd.rs`

- [ ] **Step 1: Write failing test**

`crates/roy-management/tests/session_cwd.rs`:
```rust
use roy_management::cwd::{resolve_cwd, CwdInput, CwdScope};

fn ws() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("roy-cwd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn personal_session_no_project() {
    let ws = ws();
    let p = resolve_cwd(&ws, CwdInput {
        scope: CwdScope::Personal,
        user_id: "U1".into(),
        team_id: None,
        project_id: None,
        session_id: "S1".into(),
    }).unwrap();
    assert_eq!(p, ws.join("users").join("U1").join("sessions").join("S1"));
}

#[test]
fn team_session_with_project() {
    let ws = ws();
    let p = resolve_cwd(&ws, CwdInput {
        scope: CwdScope::Team,
        user_id: "U1".into(),
        team_id: Some("T1".into()),
        project_id: Some("P1".into()),
        session_id: "S1".into(),
    }).unwrap();
    assert_eq!(p, ws.join("teams").join("T1").join("projects").join("P1").join("sessions").join("S1"));
}

#[test]
fn path_traversal_rejected() {
    let ws = ws();
    let err = resolve_cwd(&ws, CwdInput {
        scope: CwdScope::Personal,
        user_id: "../../etc".into(),
        team_id: None,
        project_id: None,
        session_id: "S1".into(),
    });
    assert!(err.is_err());
}

#[test]
fn non_uuid_id_rejected() {
    let ws = ws();
    let err = resolve_cwd(&ws, CwdInput {
        scope: CwdScope::Personal,
        user_id: "alice/bob".into(),
        team_id: None,
        project_id: None,
        session_id: "S1".into(),
    });
    assert!(err.is_err());
}
```

- [ ] **Step 2: Run test (fails on missing module)**

Run: `cargo test -p roy-management --test session_cwd`
Expected: FAIL.

- [ ] **Step 3: Write `cwd.rs`**

```rust
//! Resolve the absolute filesystem cwd of a session and validate that the
//! resulting path stays inside the workspace root. Only mkdir is performed —
//! no auto-generated CLAUDE.md or .memory/ files.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum CwdError {
    #[error("invalid id (must be UUID-shape)")]
    InvalidId,
    #[error("path escape")]
    Escape,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub enum CwdScope { Personal, Team }

pub struct CwdInput {
    pub scope: CwdScope,
    pub user_id: String,
    pub team_id: Option<String>,
    pub project_id: Option<String>,
    pub session_id: String,
}

fn is_uuid_shape(s: &str) -> bool {
    s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn resolve_cwd(workspace_dir: &Path, input: CwdInput) -> Result<PathBuf, CwdError> {
    for id in [&input.user_id, input.team_id.as_ref().unwrap_or(&String::new()), input.project_id.as_ref().unwrap_or(&String::new()), &input.session_id] {
        if !id.is_empty() && !is_uuid_shape(id) { return Err(CwdError::InvalidId); }
    }
    let root = match input.scope {
        CwdScope::Personal => workspace_dir.join("users").join(&input.user_id),
        CwdScope::Team => match &input.team_id {
            Some(t) => workspace_dir.join("teams").join(t),
            None => return Err(CwdError::InvalidId),
        }
    };
    let path = match &input.project_id {
        Some(p) => root.join("projects").join(p).join("sessions").join(&input.session_id),
        None => root.join("sessions").join(&input.session_id),
    };
    require_safe_path(workspace_dir, &path)?;
    Ok(path)
}

fn require_safe_path(workspace_dir: &Path, p: &Path) -> Result<(), CwdError> {
    let workspace = workspace_dir.canonicalize()?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
        let canonical = parent.canonicalize()?;
        if !canonical.starts_with(&workspace) {
            return Err(CwdError::Escape);
        }
    }
    Ok(())
}
```

Add `pub mod cwd;` to `crates/roy-management/src/lib.rs`.

- [ ] **Step 4: Run test**

Run: `cargo test -p roy-management --test session_cwd`
Expected: 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): resolve_cwd + path-traversal guard"
```

---

### Task C2: Wire `resolve_cwd` into `POST /sessions`

**Files:**
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/tests/auth_flow.rs`

- [ ] **Step 1: Write failing test**

Append to `auth_flow.rs`:
```rust
#[tokio::test]
async fn create_session_cwd_is_under_user_dir() {
    let (app, pool) = test_app().await;
    let alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice").await;

    let body = serde_json::to_vec(&serde_json::json!({
        "scope": "personal",
        "preset": "claude",
        "title": "hello"
    })).unwrap();
    let resp = app.clone().oneshot(
        Request::post("/sessions")
            .header("content-type","application/json")
            .header("cookie", &cookie)
            .body(Body::from(body)).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Inspect the FakeDaemon — last Spawn cwd must be under users/<alice.id>
    let spawn = state_get_last_spawn(&app);     // helper exposes the recorded ClientCommand
    assert!(spawn.cwd.to_string_lossy().contains(&format!("users/{}/sessions/", alice.id)));
    assert!(spawn.cwd.exists());
}
```

`login_as` helper:
```rust
async fn login_as(app: &axum::Router, username: &str) -> String {
    let body = serde_json::to_vec(&serde_json::json!({"username":username,"password":"test-password-1234"})).unwrap();
    let resp = app.clone().oneshot(
        Request::post("/auth/login").header("content-type","application/json").body(Body::from(body)).unwrap()
    ).await.unwrap();
    resp.headers().get("set-cookie").unwrap().to_str().unwrap().to_string()
}
```

There is already a `MockDaemonClient` in `crates/roy-management/src/roy_client.rs:171`, but it is gated behind `#[cfg(test)] pub(crate) mod mock`. Integration tests in `tests/` can't see it. **Promote it to a `test-support` feature:**

```rust
// crates/roy-management/src/roy_client.rs:163
#[cfg(any(test, feature = "test-support"))]
pub mod mock {
    // ... existing MockDaemonClient body, unchanged ...

    impl MockDaemonClient {
        pub fn last_spawn(&self) -> SpawnRequest {
            self.recorded_spawns
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("no spawn recorded")
        }
    }
}
```

Add the feature in `crates/roy-management/Cargo.toml`:
```toml
[features]
default = []
test-support = []
```

Make sure `SpawnRequest` derives `Clone` (verify at `roy_client.rs:17` — add `Clone` to its `#[derive]` if missing).

The integration test then uses:
```rust
use roy_management::roy_client::mock::MockDaemonClient;

let daemon = std::sync::Arc::new(
    MockDaemonClient::new().with_spawn("sess-1"),
);
// ... later ...
let spawn = daemon.last_spawn();
assert!(spawn.cwd.to_string_lossy().contains(&format!("users/{}/sessions/", alice.id)));
```

- [ ] **Step 2: Run test (fails)**

Run: `cargo test -p roy-management --test auth_flow create_session_cwd_is_under_user_dir -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Update `create_session` handler**

```rust
#[derive(serde::Deserialize)]
struct CreateSessionReq {
    #[serde(default = "default_scope")]
    scope: String,                                 // "personal" | "team"
    team_id: Option<String>,
    project_id: Option<String>,
    preset: String,
    #[serde(default)] model: Option<String>,
    #[serde(default)] agent_id: Option<String>,
    #[serde(default)] title: Option<String>,
}
fn default_scope() -> String { "personal".into() }

async fn create_session(
    axum::extract::Extension(AuthUser(user_id)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    Json(req): Json<CreateSessionReq>,
) -> Result<Json<SessionMeta>, ApiError> {
    let scope = match req.scope.as_str() {
        "personal" => roy_auth::Scope::Personal,
        "team" => roy_auth::Scope::Team { team_id: req.team_id.clone().ok_or(ApiError(StatusCode::BAD_REQUEST,"team_id required".into()))? },
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "invalid scope".into())),
    };
    let acl = roy_auth::Acl::new(&s.pool, &user_id);
    acl.can_access_scope(&scope).await.map_err(|_| ApiError(StatusCode::FORBIDDEN, "forbidden".into()))?;
    if let Some(pid) = &req.project_id {
        acl.can_access_project(pid).await.map_err(|_| ApiError(StatusCode::FORBIDDEN, "forbidden".into()))?;
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let cwd_scope = match scope { roy_auth::Scope::Personal => crate::cwd::CwdScope::Personal, roy_auth::Scope::Team {..} => crate::cwd::CwdScope::Team };
    let cwd = crate::cwd::resolve_cwd(&s.workspace_dir, crate::cwd::CwdInput {
        scope: cwd_scope,
        user_id: user_id.clone(),
        team_id: req.team_id.clone(),
        project_id: req.project_id.clone(),
        session_id: session_id.clone(),
    }).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    std::fs::create_dir_all(&cwd).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    s.meta.upsert_session_meta(&SessionMeta {
        session_id: session_id.clone(),
        project_id: req.project_id.clone(),
        agent_id: req.agent_id.clone(),
        agent_name: None,
        display_label: req.title.clone(),
        created_by: user_id.clone(),
        team_id: req.team_id.clone(),
        tags: Default::default(),
        created_at: chrono::Utc::now().timestamp(),
    }).await.map_err(map_meta_err)?;

    let spawn = SpawnRequest {
        session_id: session_id.clone(),
        cwd,
        preset: req.preset,
        model: req.model,
        ..Default::default()
    };
    s.daemon.spawn(spawn).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;

    Ok(Json(SessionMeta { session_id, project_id: req.project_id, agent_id: req.agent_id, agent_name: None, display_label: req.title, created_by: user_id, team_id: req.team_id, tags: Default::default(), created_at: chrono::Utc::now().timestamp() }))
}
```

- [ ] **Step 4: Run test**

Run: `cargo test -p roy-management --test auth_flow create_session_cwd_is_under_user_dir -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): per-scope cwd resolution in POST /sessions"
```

---

### Task C3: ACL test — non-member can't create team session

**Files:**
- Modify: `crates/roy-management/tests/acl.rs` (create)

- [ ] **Step 1: Write the test**

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use roy_auth::test_support::{make_team, make_user};
// reuse test_app + login_as from auth_flow.rs via mod common — for now duplicate the helpers if too cumbersome.

#[tokio::test]
async fn non_member_cannot_create_team_session() {
    // Setup
    // ... (similar to auth_flow.rs::test_app)
    // alice owns team T; bob is not a member.
    // bob logs in, tries to POST /sessions {scope: "team", teamId: T.id} → 403.
}
```

Implement using the same fixture pattern (duplicate `test_app`/`login_as` or extract into `tests/common/mod.rs`).

- [ ] **Step 2: Run test**

Run: `cargo test -p roy-management --test acl`
Expected: PASS (3rd assertion is `403 FORBIDDEN`).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-management
git commit -m "test(roy-management): ACL guard on team session create"
```

---

### **Checkpoint C** — Per-scope cwd works end-to-end

Run: `cargo test --workspace --no-fail-fast`
Expected: green.

---

## Phase D — Teams + invites HTTP endpoints

### Task D1: `GET /teams` + `POST /teams`

**Files:**
- Modify: `crates/roy-management/src/auth.rs` (extend `router()`)

- [ ] **Step 1: Write failing test in `auth_flow.rs`**

```rust
#[tokio::test]
async fn create_team_then_list_returns_it() {
    let (app, pool) = test_app().await;
    let _ = make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice").await;

    let body = serde_json::to_vec(&serde_json::json!({"name":"eng"})).unwrap();
    let resp = app.clone().oneshot(
        Request::post("/teams")
            .header("content-type","application/json").header("cookie",&cookie)
            .body(Body::from(body)).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app.oneshot(
        Request::get("/teams").header("cookie",&cookie).body(Body::empty()).unwrap()
    ).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v[0]["name"], "eng");
    assert_eq!(v[0]["role"], "owner");
}
```

- [ ] **Step 2: Add handlers to `auth.rs`**

```rust
async fn list_teams(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(state): State<AppState>,
) -> Response {
    let teams = TeamStore::new(state.pool.clone()).list_for_user(&uid).await.unwrap_or_default();
    Json(teams).into_response()
}

#[derive(serde::Deserialize)]
struct CreateTeamReq { name: String, #[serde(default)] description: Option<String> }

async fn create_team(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<CreateTeamReq>,
) -> Response {
    let team = TeamStore::new(state.pool.clone()).create(roy_auth::NewTeam { name: req.name, description: req.description }, &uid).await;
    match team {
        Ok(t) => Json(t).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":"internal"}))).into_response(),
    }
}

async fn delete_team(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(state): State<AppState>,
    axum::extract::Path(team_id): axum::extract::Path<String>,
) -> Response {
    if let Err(_) = roy_auth::Acl::new(&state.pool, &uid).can_admin_team(&team_id).await {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error":"forbidden"}))).into_response();
    }
    TeamStore::new(state.pool.clone()).delete(&team_id).await.ok();
    StatusCode::NO_CONTENT.into_response()
}
```

These need to be added to the **protected** router (they live behind `require_user`). Move team routes into `http.rs::router` instead of `auth::router`:

```rust
.route("/teams", get(auth::list_teams).post(auth::create_team))
.route("/teams/{id}", axum::routing::delete(auth::delete_team))
```

Export the handlers from `auth.rs` (`pub async fn list_teams ...`).

- [ ] **Step 3: Run test**

Run: `cargo test -p roy-management --test auth_flow create_team_then_list_returns_it -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): GET/POST/DELETE /teams handlers"
```

---

### Task D2: Invite endpoints

**Files:**
- Modify: `crates/roy-management/src/auth.rs`
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/tests/auth_flow.rs`

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn invite_create_then_accept_adds_member() {
    let (app, pool) = test_app().await;
    let alice = make_user(&pool, "alice").await;
    let _bob = make_user(&pool, "bob").await;
    let cookie_a = login_as(&app, "alice").await;
    let cookie_b = login_as(&app, "bob").await;

    // alice creates team
    let body = serde_json::to_vec(&serde_json::json!({"name":"eng"})).unwrap();
    let resp = app.clone().oneshot(Request::post("/teams").header("content-type","application/json").header("cookie",&cookie_a).body(Body::from(body)).unwrap()).await.unwrap();
    let team: serde_json::Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let team_id = team["id"].as_str().unwrap().to_string();

    // alice creates invite
    let body = serde_json::to_vec(&serde_json::json!({"teamId":team_id})).unwrap();
    let resp = app.clone().oneshot(Request::post("/auth/invites").header("content-type","application/json").header("cookie",&cookie_a).body(Body::from(body)).unwrap()).await.unwrap();
    let inv: serde_json::Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let token = inv["token"].as_str().unwrap().to_string();

    // bob accepts
    let body = serde_json::to_vec(&serde_json::json!({"token":token})).unwrap();
    let resp = app.clone().oneshot(Request::post("/auth/accept-invite").header("content-type","application/json").header("cookie",&cookie_b).body(Body::from(body)).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // /auth/me for bob now shows the team
    let resp = app.oneshot(Request::get("/auth/me").header("cookie",&cookie_b).body(Body::empty()).unwrap()).await.unwrap();
    let me: serde_json::Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(me["teams"][0]["id"], team_id);
}
```

- [ ] **Step 2: Add handlers in `auth.rs`**

```rust
pub async fn create_invite(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<CreateInviteReq>,
) -> Response {
    if let Err(_) = roy_auth::Acl::new(&state.pool, &uid).can_admin_team(&req.team_id).await {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error":"forbidden"}))).into_response();
    }
    let inv = roy_auth::InviteStore::new(state.pool.clone()).create(&req.team_id, &uid, req.expires_at).await;
    match inv {
        Ok(i) => Json(serde_json::json!({ "token": i.token, "team_id": i.team_id })).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":"internal"}))).into_response(),
    }
}

pub async fn accept_invite(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<AcceptInviteReq>,
) -> Response {
    let inv = roy_auth::InviteStore::new(state.pool.clone());
    match inv.accept(&req.token, &uid).await {
        Ok(team_id) => Json(serde_json::json!({"team_id": team_id})).into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"invite invalid"}))).into_response(),
    }
}

#[derive(serde::Deserialize)] struct CreateInviteReq { team_id: String, #[serde(default)] expires_at: Option<i64> }
#[derive(serde::Deserialize)] struct AcceptInviteReq { token: String }
```

Mount in `http.rs::router`:
```rust
.route("/auth/invites", post(auth::create_invite))
.route("/auth/accept-invite", post(auth::accept_invite))
```

Both are inside the protected layer (need a logged-in user).

- [ ] **Step 3: Run test**

Run: `cargo test -p roy-management --test auth_flow invite_create_then_accept_adds_member -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): POST /auth/invites + /auth/accept-invite"
```

---

### **Checkpoint D** — Teams + invites complete

Run: `cargo test --workspace --no-fail-fast`
Expected: green.

---

## Phase E — Commands discovery

### Task E1: Skill scanner

**Files:**
- Create: `crates/roy-management/src/commands.rs`
- Modify: `crates/roy-management/src/lib.rs`
- Modify: `crates/roy-management/tests/commands_discovery.rs`

- [ ] **Step 1: Write failing test**

`crates/roy-management/tests/commands_discovery.rs`:
```rust
use roy_management::commands::list_commands_from;

#[tokio::test]
async fn scans_user_skills_dir() {
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().join(".claude/skills/review");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: review\ndescription: Review a PR\n---\n\nBody.",
    ).unwrap();
    let out = list_commands_from(dir.path(), &[]).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "review");
    assert_eq!(out[0].description, "Review a PR");
    assert_eq!(out[0].source, "user");
}
```

- [ ] **Step 2: Run test (fails)**

Run: `cargo test -p roy-management --test commands_discovery`
Expected: FAIL.

- [ ] **Step 3: Write `commands.rs`**

```rust
//! Filesystem-based discovery of slash commands. Two sources:
//!  - <HOME>/.claude/skills/<name>/SKILL.md            ("user" source)
//!  - <HOME>/.claude/plugins/marketplaces/<m>/{plugins,external_plugins}/<p>/skills/<name>/SKILL.md
//!    (source = "<p>@<m>"), gated by enabledPlugins in ~/.claude/settings.json.
//!
//! Each SKILL.md has YAML frontmatter with `name` and `description`.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub source: String,
}

pub async fn list_commands_from(home: &Path, enabled_plugins: &[String]) -> Vec<CommandInfo> {
    let mut out = scan_dir(&home.join(".claude/skills"), "user").await;
    for plugin in enabled_plugins {
        // expected shape "<plugin>@<marketplace>"
        let Some((p, m)) = plugin.split_once('@') else { continue; };
        let dir = home
            .join(".claude/plugins/marketplaces")
            .join(m).join("plugins").join(p).join("skills");
        out.extend(scan_dir(&dir, plugin).await);
        let dir2 = home
            .join(".claude/plugins/marketplaces")
            .join(m).join("external_plugins").join(p).join("skills");
        out.extend(scan_dir(&dir2, plugin).await);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

async fn scan_dir(dir: &Path, source: &str) -> Vec<CommandInfo> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else { return out };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let skill_md = entry.path().join("SKILL.md");
        let Ok(contents) = tokio::fs::read_to_string(&skill_md).await else { continue };
        let Some((name, desc)) = parse_frontmatter(&contents) else { continue };
        out.push(CommandInfo { name, description: desc, source: source.into() });
    }
    out
}

fn parse_frontmatter(s: &str) -> Option<(String, String)> {
    let s = s.strip_prefix("---\n")?;
    let end = s.find("\n---")?;
    let body = &s[..end];
    let (mut name, mut desc) = (None, None);
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            desc = Some(rest.trim().trim_matches('"').to_string());
        }
    }
    Some((name?, desc?))
}
```

Add `pub mod commands;` to `lib.rs`.

- [ ] **Step 4: Run test**

Run: `cargo test -p roy-management --test commands_discovery -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): SKILL.md scanner (user + plugin marketplaces)"
```

---

### Task E2: `GET /commands` endpoint + enabledPlugins reader + cache

**Files:**
- Modify: `crates/roy-management/src/commands.rs`
- Modify: `crates/roy-management/src/http.rs`

- [ ] **Step 1: Add cache + enabledPlugins reader**

```rust
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct CommandsCache { inner: Mutex<Option<(Instant, Vec<CommandInfo>)>> }
const TTL: Duration = Duration::from_secs(30);

impl CommandsCache {
    pub async fn get(&self) -> Vec<CommandInfo> {
        {
            let g = self.inner.lock().unwrap();
            if let Some((ts, ref v)) = *g { if ts.elapsed() < TTL { return v.clone(); } }
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let plugins = read_enabled_plugins(&home).unwrap_or_default();
        let v = list_commands_from(&home, &plugins).await;
        let mut g = self.inner.lock().unwrap();
        *g = Some((Instant::now(), v.clone()));
        v
    }
}

fn read_enabled_plugins(home: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(home.join(".claude/settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let map = v.get("enabledPlugins")?.as_object()?;
    Some(map.iter()
        .filter(|(_, v)| v.as_bool() == Some(true))
        .map(|(k, _)| k.clone())
        .collect())
}
```

Add `dirs = "5"` to `Cargo.toml`.

- [ ] **Step 2: Add cache to `AppState`**

```rust
// state.rs
pub commands_cache: std::sync::Arc<crate::commands::CommandsCache>,
```

Initialize in `lib.rs::run`.

- [ ] **Step 3: Add handler in `http.rs`**

```rust
.route("/commands", get(list_commands))
// ...
async fn list_commands(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::commands::CommandInfo>>, ApiError> {
    Ok(Json(state.commands_cache.get().await))
}
```

- [ ] **Step 4: Add smoke test in `commands_discovery.rs`**

```rust
#[tokio::test]
async fn handler_returns_commands() {
    // setup test_app with HOME pointing at a tempdir that has a SKILL.md
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    let skills = home.path().join(".claude/skills/quicksum");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("SKILL.md"), "---\nname: quicksum\ndescription: Sum up\n---\n").unwrap();

    let (app, _pool) = super::test_app::test_app().await;     // shared helper
    let cookie = super::test_app::login_as(&app, "alice").await;
    let resp = app.oneshot(
        Request::get("/commands").header("cookie", &cookie).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
```

(Helper `super::test_app::test_app` may need lifting to a shared `tests/common/mod.rs`.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p roy-management --test commands_discovery -- --nocapture`
Expected: 2 PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management
git commit -m "feat(roy-management): GET /commands with 30s cache + enabledPlugins gate"
```

---

### **Checkpoint E** — Commands discovery works

Run: `cargo test --workspace --no-fail-fast`
Expected: green.

---

## Phase F — `roy-gateway` WS auth

### Task F1: Rewrite `ws_auth_callback` to verify JWT subprotocol

**Files:**
- Modify: `crates/roy-gateway/Cargo.toml`
- Modify: `crates/roy-gateway/src/ws.rs`
- Modify: `crates/roy-gateway/src/lib.rs`
- Create: `crates/roy-gateway/tests/ws_auth.rs`

- [ ] **Step 1: Add dep**

```toml
roy-auth = { path = "../roy-auth" }
```

- [ ] **Step 2: Write failing test**

`crates/roy-gateway/tests/ws_auth.rs`:
```rust
use roy_auth::test_support::{issue_jwt, TEST_JWT_SECRET};
use roy_gateway::ws::ws_auth_callback_inner;

#[test]
fn valid_jwt_extracts_user_id() {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let token = issue_jwt("U-1");
    let header = format!("roy-jwt,{token}");
    let uid = ws_auth_callback_inner(&header).unwrap();
    assert_eq!(uid, "U-1");
}

#[test]
fn missing_marker_rejected() {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let token = issue_jwt("U-1");
    assert!(ws_auth_callback_inner(&token).is_err());
}

#[test]
fn tampered_jwt_rejected() {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let mut token = issue_jwt("U-1");
    let last = token.pop().unwrap();
    token.push(if last == 'A' { 'B' } else { 'A' });
    let header = format!("roy-jwt,{token}");
    assert!(ws_auth_callback_inner(&header).is_err());
}
```

- [ ] **Step 3: Rewrite `ws.rs`**

Replace the existing `ws_auth_callback`:
```rust
pub fn ws_auth_callback_inner(header_value: &str) -> Result<String, roy_auth::JwtError> {
    roy_auth::verify_ws_protocol(header_value)
}

fn ws_auth_callback() -> impl FnOnce(&Request, Response) -> std::result::Result<Response, ErrorResponse> {
    move |req, mut resp| {
        let provided = req.headers().get(WS_TOKEN_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
        let _user_id = ws_auth_callback_inner(provided).map_err(|_| {
            http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(Some("invalid roy ws token".into()))
                .expect("valid http response")
        })?;
        // Reply with just "roy-jwt" as the selected subprotocol — the token itself isn't echoed back.
        resp.headers_mut().insert(WS_TOKEN_HEADER, http::HeaderValue::from_static("roy-jwt"));
        Ok(resp)
    }
}
```

Drop the `load_or_create_ws_token` path and the `Arc<String>` plumbing.

- [ ] **Step 4: Update `lib.rs`**

Remove the token-file argument; the JWT is self-contained.

- [ ] **Step 5: Run tests**

Run: `cargo test -p roy-gateway --test ws_auth -- --nocapture`
Expected: 3 PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-gateway
git commit -m "feat(roy-gateway): WS handshake authenticates via JWT subprotocol"
```

---

### Task F2: Remove the shared-token file path

**Files:**
- Modify: `crates/roy-gateway/src/lib.rs`
- Modify: `crates/roy-gateway/src/ws.rs`

- [ ] **Step 1: Delete `load_or_create_ws_token` + related plumbing.**

Remove the `token_path` argument from any function that no longer needs it. Update call sites in `lib.rs` and `roy-cli` (the `gateway` subcommand) accordingly.

- [ ] **Step 2: Compile**

Run: `cargo build --workspace --all-targets`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-gateway crates/roy-cli
git commit -m "chore(roy-gateway): drop shared-token file (replaced by JWT)"
```

---

### **Checkpoint F** — WS auth uses JWT

Run: `cargo test --workspace --no-fail-fast`
Expected: green.

---

## Phase G — `roy-cli` auth subcommands

### Task G1: `roy auth login` + cookie file

**Files:**
- Create: `crates/roy-cli/src/auth.rs`
- Modify: `crates/roy-cli/Cargo.toml`
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Add deps**

```toml
reqwest = { version = "0.12", default-features = false, features = ["json","rustls-tls"] }
rpassword = "7"
```

- [ ] **Step 2: Write `auth.rs`**

```rust
//! `roy auth login | whoami | reset` — interactive CLI for the HTTP API.

use std::path::PathBuf;

pub fn cookie_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("roy").join("cookie")
}

fn ensure_dir(p: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
    Ok(())
}

pub async fn login(api: &str) -> anyhow::Result<()> {
    let username = rpassword::prompt_password("username: ")?;     // hidden — fine since rpassword reads a line
    let password = rpassword::prompt_password("password: ")?;
    let client = reqwest::Client::new();
    let resp = client.post(format!("{api}/auth/login"))
        .json(&serde_json::json!({"username":username.trim(),"password":password}))
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("login failed: {}", resp.status());
    }
    let cookie = resp.headers().get(reqwest::header::SET_COOKIE)
        .ok_or_else(|| anyhow::anyhow!("no set-cookie"))?
        .to_str()?.to_string();
    let path = cookie_path();
    ensure_dir(&path)?;
    std::fs::write(&path, cookie)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&path)?.permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(&path, p)?;
    }
    println!("Logged in. Cookie saved to {}", path.display());
    Ok(())
}

pub async fn whoami(api: &str) -> anyhow::Result<()> {
    let cookie = std::fs::read_to_string(cookie_path())?;
    let client = reqwest::Client::new();
    let resp = client.get(format!("{api}/auth/me")).header(reqwest::header::COOKIE, cookie).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("not logged in: {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}
```

- [ ] **Step 3: Wire into `main.rs`**

Add `Auth { ... }` to the existing clap subcommand enum:
```rust
#[derive(clap::Subcommand)]
enum Cmd {
    // ...existing variants...
    Auth(AuthArgs),
}

#[derive(clap::Args)]
struct AuthArgs {
    #[command(subcommand)]
    cmd: AuthCmd,
    #[arg(long, env = "ROY_MANAGEMENT_URL", default_value = "http://127.0.0.1:8079")]
    api: String,
}

#[derive(clap::Subcommand)]
enum AuthCmd { Login, Whoami }
```

Dispatch:
```rust
Cmd::Auth(args) => match args.cmd {
    AuthCmd::Login => roy_cli::auth::login(&args.api).await?,
    AuthCmd::Whoami => roy_cli::auth::whoami(&args.api).await?,
},
```

(Export `pub mod auth;` from `roy-cli/src/lib.rs` if needed.)

- [ ] **Step 4: Smoke test manually**

The login flow requires a running daemon + roy-management — there is no automated test here. Document the manual smoke in the checklist (Phase H).

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli
git commit -m "feat(roy-cli): roy auth login + whoami subcommands"
```

---

### Task G2: `roy auth reset`

**Files:**
- Modify: `crates/roy-cli/src/auth.rs`
- Modify: `crates/roy-cli/src/main.rs`
- Modify: `crates/roy-cli/Cargo.toml`

- [ ] **Step 1: Add `roy-auth` dep**

```toml
roy-auth = { path = "../roy-auth" }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio","sqlite"] }
```

- [ ] **Step 2: Add handler**

```rust
pub async fn reset_password(username: &str) -> anyhow::Result<()> {
    let new_pw = rpassword::prompt_password("new password: ")?;
    if new_pw.trim().len() < 8 { anyhow::bail!("password too short"); }
    let db = roy_agents::default_db_path();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&db).create_if_missing(false))
        .await?;
    let user = roy_auth::UserStore::new(pool.clone()).get_by_username(username).await?;
    roy_auth::UserStore::new(pool).set_password(&user.id, new_pw.trim()).await?;
    println!("Password updated for {username}");
    Ok(())
}
```

- [ ] **Step 3: Wire into clap**

```rust
enum AuthCmd { Login, Whoami, Reset { username: String } }

// dispatch:
AuthCmd::Reset { username } => roy_cli::auth::reset_password(&username).await?,
```

- [ ] **Step 4: Compile**

Run: `cargo build --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli
git commit -m "feat(roy-cli): roy auth reset <username> via direct DB access"
```

---

### **Checkpoint G** — CLI auth commands work

Run: `cargo test --workspace --no-fail-fast`
Expected: green.

---

## Phase H — Smoke + docs

### Task H1: Smoke checklist run

- [ ] Start `roy-management` with `ROY_JWT_SECRET=$(openssl rand -hex 32)` — bootstrap-root prints a password to stderr.
- [ ] `curl -c jar -X POST -H 'content-type: application/json' -d '{"username":"root","password":"<from-stderr>"}' http://127.0.0.1:8079/auth/login` → 200 + `Set-Cookie`.
- [ ] `curl -b jar http://127.0.0.1:8079/auth/me` → JSON profile.
- [ ] `curl -b jar -X POST -H 'content-type: application/json' -d '{"scope":"personal","preset":"claude"}' http://127.0.0.1:8079/sessions` → 200; cwd at `~/.roy/workspace/users/<uid>/sessions/<sid>/` exists.
- [ ] `curl -b jar http://127.0.0.1:8079/commands` returns SKILL.md entries.
- [ ] `roy auth login` interactive prompt writes `~/.config/roy/cookie`.
- [ ] `roy auth whoami` prints profile.
- [ ] Start `roy-gateway` and connect via WebSocket with `Sec-WebSocket-Protocol: roy-jwt,<JWT>`. Invalid JWT → 401 close.

- [ ] **Commit any smoke-fixes uncovered.**

---

### Task H2: Update `CLAUDE.md` (only if behaviour changed)

- [ ] **Step 1: Add a short paragraph to `CLAUDE.md` under «What this is»** describing the new `roy-auth` crate, the `~/.roy/workspace/{users,teams}/...` layout, and how `roy-management` now requires `ROY_JWT_SECRET`.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: describe roy-auth, per-scope cwd, and JWT requirement"
```

---

## Self-review

After completing all phases:

- [ ] `cargo fmt --all -- --check` — green.
- [ ] `cargo build --workspace --all-targets` — green.
- [ ] `cargo test --workspace --no-fail-fast` — green.
- [ ] All spec sections implemented:
  - [ ] User entity + bcrypt + JWT (Tasks A3, A4, A5)
  - [ ] Teams + invites (Tasks A7, A8, D1, D2)
  - [ ] Per-scope cwd (Tasks C1, C2)
  - [ ] Commands discovery (Tasks E1, E2)
  - [ ] WS JWT handshake (Tasks F1, F2)
  - [ ] CLI auth commands (Tasks G1, G2)
  - [ ] Bootstrap-root (Task B3)
  - [ ] Rate limit + anti-enumeration (Task B6 + B4 dummy hash)
  - [ ] Path-traversal guard (Task C1)
  - [ ] ACL helpers (Task A9)
- [ ] Open the spec side-by-side and confirm no requirement was missed.
