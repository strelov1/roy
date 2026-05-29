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
            // Everything inserted is already in scope; no DB-side defaults or
            // triggers mutate the row, so build it directly instead of re-SELECTing.
            Ok(_) => Ok(ChannelBinding {
                id,
                owner_id: owner_id.to_string(),
                channel_kind: CHANNEL_TELEGRAM.to_string(),
                connection_id: new.connection_id.clone(),
                agent_slug: new.agent_slug.clone(),
                agent_scope: new.agent_scope.clone(),
                session_strategy: new.session_strategy.clone(),
                idle_timeout_secs: new.idle_timeout_secs,
                allowed_user_ids: new.allowed_user_ids.clone(),
                enabled: true,
                created_at: now,
                updated_at: now,
            }),
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

// ---------------- HTTP ----------------

use axum::{
    extract::{Path as AxPath, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use std::path::Path;

use crate::auth::AuthUser;
use crate::http::ApiError;
use crate::state::AppState;
use roy_protocol::channel::{SessionStrategyWire, TelegramSource};

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(id) => ApiError(StatusCode::NOT_FOUND, format!("not found: {id}")),
            StoreError::Invalid(m) => ApiError(StatusCode::BAD_REQUEST, m),
            StoreError::Db(e) => {
                tracing::error!(error = %e, "channel_bindings db error");
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        }
    }
}

/// Authenticated CRUD, mounted behind `require_user`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/channel-bindings", get(list_handler).post(create_handler))
        .route(
            "/channel-bindings/{id}",
            get(get_handler).delete(delete_handler),
        )
}

/// Internal source list, mounted behind `require_internal_token`.
pub fn internal_router() -> Router<AppState> {
    Router::new().route("/internal/telegram-sources", get(internal_telegram_sources))
}

async fn list_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
) -> Result<Json<Vec<ChannelBinding>>, ApiError> {
    Ok(Json(s.channel_bindings.list_by_owner(&uid).await?))
}

async fn get_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Json<ChannelBinding>, ApiError> {
    Ok(Json(s.channel_bindings.get(&uid, &id).await?))
}

async fn delete_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<StatusCode, ApiError> {
    s.channel_bindings.delete(&uid, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    Json(new): Json<NewChannelBinding>,
) -> Result<(StatusCode, Json<ChannelBinding>), ApiError> {
    // Validate strategy.
    match new.session_strategy.as_str() {
        "ephemeral" | "persistent_one" => {}
        "per_sender_sticky" => {
            if new.idle_timeout_secs.is_none() {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "per_sender_sticky requires idle_timeout_secs".into(),
                ));
            }
        }
        other => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                format!("unknown session_strategy '{other}'"),
            ))
        }
    }
    // Validate the connection: owned, telegram_bot, has a non-empty bot_token.
    let conn = match s.connections.get(&uid, &new.connection_id).await {
        Ok(c) => c,
        Err(crate::connections::StoreError::Db(e)) => {
            tracing::error!(error = %e, "channel binding create: connection lookup db error");
            return Err(ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".into(),
            ));
        }
        Err(_) => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "unknown connection".into(),
            ))
        }
    };
    if conn.kind != crate::connections::KIND_TELEGRAM_BOT {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "connection is not a telegram_bot".into(),
        ));
    }
    let has_token = conn
        .secrets
        .as_ref()
        .and_then(|v| v.get("bot_token"))
        .and_then(|v| v.as_str())
        .is_some_and(|t| !t.is_empty());
    if !has_token {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "connection has no bot_token secret".into(),
        ));
    }
    // Validate the agent resolves in the requested scope.
    let dir = crate::agents::agent_scope_dir(&s.workspace_dir, &uid, &new.agent_scope)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "invalid agent_scope".into()))?;
    if crate::agents::read_agent_persona(&dir, &new.agent_slug)
        .await
        .is_none()
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("agent '{}' not found in scope", new.agent_slug),
        ));
    }
    let b = s.channel_bindings.create(&uid, &new).await?;
    Ok((StatusCode::CREATED, Json(b)))
}

async fn internal_telegram_sources(State(s): State<AppState>) -> Json<Vec<TelegramSource>> {
    Json(resolve_telegram_sources(&s.channel_bindings, &s.connections, &s.workspace_dir).await)
}

/// Resolve all enabled Telegram bindings to self-contained sources. Bindings
/// whose connection or agent fails to resolve are skipped with a warning.
pub(crate) async fn resolve_telegram_sources(
    bindings: &Store,
    connections: &crate::connections::Store,
    workspace_dir: &Path,
) -> Vec<TelegramSource> {
    let rows = match bindings.list_enabled_telegram().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "listing telegram bindings");
            return vec![];
        }
    };
    let mut out = Vec::new();
    for b in rows {
        let conn = match connections.get(&b.owner_id, &b.connection_id).await {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(
                    binding = b.id,
                    "telegram binding: connection gone; skipping"
                );
                continue;
            }
        };
        let token = conn
            .secrets
            .as_ref()
            .and_then(|v| v.get("bot_token"))
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty());
        let Some(token) = token else {
            tracing::warn!(binding = b.id, "telegram binding: no bot_token; skipping");
            continue;
        };
        let Some(dir) = crate::agents::agent_scope_dir(workspace_dir, &b.owner_id, &b.agent_scope)
        else {
            tracing::warn!(
                binding = b.id,
                scope = b.agent_scope,
                "bad agent_scope; skipping"
            );
            continue;
        };
        let Some((harness, _model, body)) =
            crate::agents::read_agent_persona(&dir, &b.agent_slug).await
        else {
            tracing::warn!(
                binding = b.id,
                slug = b.agent_slug,
                "agent unresolved; skipping"
            );
            continue;
        };
        out.push(TelegramSource {
            source_id: format!("tg:{}", b.connection_id),
            bot_token: token.to_string(),
            agent_slug: b.agent_slug,
            harness,
            system_prompt: Some(body),
            session_strategy: strategy_to_wire(&b.session_strategy, b.idle_timeout_secs),
            allowed_user_ids: b.allowed_user_ids,
        });
    }
    out
}

fn strategy_to_wire(name: &str, idle: Option<i64>) -> SessionStrategyWire {
    match name {
        "persistent_one" => SessionStrategyWire::PersistentOne,
        "per_sender_sticky" => SessionStrategyWire::PerSenderSticky {
            idle_timeout_secs: idle.unwrap_or(3600).max(0) as u64,
        },
        _ => SessionStrategyWire::Ephemeral,
    }
}
