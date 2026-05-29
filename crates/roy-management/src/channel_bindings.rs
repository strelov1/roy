//! Channel→agent bindings: which agent persona a Telegram bot runs, and with
//! what session strategy. Web-UI managed (CRUD), read by `roy-inbound` via the
//! internal endpoint (see `internal_telegram_sources`). Owner is always a user.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

pub const CHANNEL_TELEGRAM: &str = "telegram";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChannelBinding {
    pub id: String,
    pub owner_id: String,
    pub channel_kind: String,
    pub connection_id: String,
    pub agent_slug: String,
    pub agent_scope: String,
    pub session_strategy: String,
    pub idle_timeout_secs: Option<i64>,
    pub allowed_user_ids: Vec<i64>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Request body for `POST /channel-bindings`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewChannelBinding {
    pub connection_id: String,
    pub agent_slug: String,
    /// "user" | "team:<team_id>"
    pub agent_scope: String,
    #[serde(default = "default_strategy")]
    pub session_strategy: String,
    #[serde(default)]
    pub idle_timeout_secs: Option<i64>,
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
}

fn default_strategy() -> String {
    "per_sender_sticky".to_string()
}

#[derive(sqlx::FromRow)]
struct BindingRow {
    id: String,
    owner_id: String,
    channel_kind: String,
    connection_id: String,
    agent_slug: String,
    agent_scope: String,
    session_strategy: String,
    idle_timeout_secs: Option<i64>,
    allowed_user_ids: Option<String>,
    enabled: i64,
    created_at: i64,
    updated_at: i64,
}

fn row_to_binding(r: BindingRow) -> ChannelBinding {
    let allowed_user_ids = r
        .allowed_user_ids
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
        .unwrap_or_default();
    ChannelBinding {
        id: r.id,
        owner_id: r.owner_id,
        channel_kind: r.channel_kind,
        connection_id: r.connection_id,
        agent_slug: r.agent_slug,
        agent_scope: r.agent_scope,
        session_strategy: r.session_strategy,
        idle_timeout_secs: r.idle_timeout_secs,
        allowed_user_ids,
        enabled: r.enabled != 0,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("binding not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

const SELECT_COLS: &str = "id, owner_id, channel_kind, connection_id, agent_slug, agent_scope, \
     session_strategy, idle_timeout_secs, allowed_user_ids, enabled, created_at, updated_at";

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a Telegram binding. Caller has already validated the connection,
    /// agent, and strategy. `allowed_user_ids` is stored as a JSON array.
    pub async fn create(
        &self,
        owner_id: &str,
        new: &NewChannelBinding,
    ) -> Result<ChannelBinding, StoreError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let allowed = serde_json::to_string(&new.allowed_user_ids)
            .map_err(|e| StoreError::Invalid(format!("allowed_user_ids: {e}")))?;
        let res = sqlx::query(
            "INSERT INTO channel_bindings
             (id, owner_id, channel_kind, connection_id, agent_slug, agent_scope,
              session_strategy, idle_timeout_secs, allowed_user_ids, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(&id)
        .bind(owner_id)
        .bind(CHANNEL_TELEGRAM)
        .bind(&new.connection_id)
        .bind(&new.agent_slug)
        .bind(&new.agent_scope)
        .bind(&new.session_strategy)
        .bind(new.idle_timeout_secs)
        .bind(&allowed)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => self.get(owner_id, &id).await,
            Err(sqlx::Error::Database(d)) if d.is_unique_violation() => Err(StoreError::Invalid(
                "this bot is already bound to an agent".to_string(),
            )),
            Err(e) => Err(StoreError::Db(e)),
        }
    }

    pub async fn list_by_owner(&self, owner_id: &str) -> Result<Vec<ChannelBinding>, StoreError> {
        let rows: Vec<BindingRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLS} FROM channel_bindings WHERE owner_id = ? ORDER BY created_at DESC"
        ))
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_binding).collect())
    }

    pub async fn get(&self, owner_id: &str, id: &str) -> Result<ChannelBinding, StoreError> {
        let row: Option<BindingRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLS} FROM channel_bindings WHERE owner_id = ? AND id = ?"
        ))
        .bind(owner_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_binding)
            .ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    pub async fn delete(&self, owner_id: &str, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM channel_bindings WHERE owner_id = ? AND id = ?")
            .bind(owner_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// All enabled Telegram bindings across every owner. Used by the internal
    /// endpoint to build the source list for `roy-inbound`.
    pub async fn list_enabled_telegram(&self) -> Result<Vec<ChannelBinding>, StoreError> {
        let rows: Vec<BindingRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLS} FROM channel_bindings \
             WHERE channel_kind = ? AND enabled = 1"
        ))
        .bind(CHANNEL_TELEGRAM)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_binding).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy_auth::test_support::make_user;

    async fn setup_pool() -> SqlitePool {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        roy_auth::apply_migrations(&pool).await.unwrap();
        std::mem::forget(dir);
        pool
    }

    async fn make_conn(pool: &SqlitePool, owner_id: &str) -> String {
        let store = crate::connections::Store::new(pool.clone());
        let c = store
            .create_custom(
                owner_id,
                crate::connections::NewConnectionCustom {
                    name: "My Bot".into(),
                    kind: crate::connections::KIND_TELEGRAM_BOT.into(),
                    config: serde_json::json!({}),
                    secrets: Some(serde_json::json!({"bot_token": "123:abc"})),
                    description: None,
                },
            )
            .await
            .unwrap();
        c.id
    }

    #[tokio::test]
    async fn create_list_get_delete() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let conn_id = make_conn(&pool, &user.id).await;
        let store = Store::new(pool.clone());

        let b = store
            .create(
                &user.id,
                &NewChannelBinding {
                    connection_id: conn_id.clone(),
                    agent_slug: "support-l1".into(),
                    agent_scope: "user".into(),
                    session_strategy: "per_sender_sticky".into(),
                    idle_timeout_secs: Some(3600),
                    allowed_user_ids: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(b.connection_id, conn_id);
        assert!(b.enabled);

        assert_eq!(store.list_by_owner(&user.id).await.unwrap().len(), 1);
        assert_eq!(store.list_enabled_telegram().await.unwrap().len(), 1);
        assert_eq!(store.get(&user.id, &b.id).await.unwrap().id, b.id);

        store.delete(&user.id, &b.id).await.unwrap();
        assert!(store.list_by_owner(&user.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn allowed_user_ids_round_trip() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let conn_id = make_conn(&pool, &user.id).await;
        let store = Store::new(pool.clone());

        let b = store
            .create(
                &user.id,
                &NewChannelBinding {
                    connection_id: conn_id,
                    agent_slug: "support-l1".into(),
                    agent_scope: "user".into(),
                    session_strategy: "per_sender_sticky".into(),
                    idle_timeout_secs: None,
                    allowed_user_ids: vec![123, 456],
                },
            )
            .await
            .unwrap();
        assert_eq!(b.allowed_user_ids, vec![123, 456]);

        let via_get = store.get(&user.id, &b.id).await.unwrap();
        assert_eq!(via_get.allowed_user_ids, vec![123, 456]);

        let via_list = store.list_by_owner(&user.id).await.unwrap();
        assert_eq!(via_list[0].allowed_user_ids, vec![123, 456]);
    }

    #[tokio::test]
    async fn one_bot_one_binding() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let conn_id = make_conn(&pool, &user.id).await;
        let store = Store::new(pool.clone());
        let new = NewChannelBinding {
            connection_id: conn_id,
            agent_slug: "a".into(),
            agent_scope: "user".into(),
            session_strategy: "ephemeral".into(),
            idle_timeout_secs: None,
            allowed_user_ids: vec![],
        };
        store.create(&user.id, &new).await.unwrap();
        let err = store.create(&user.id, &new).await.unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)));
    }
}
