//! User-owned MCP connections: types, store, and HTTP handlers.
//!
//! Owner is always a user (no team-shared connections in MVP). Slugs are
//! derived from `name` and made unique per-owner by suffixing (`-2`, `-3`,
//! ...).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One stored connection. `config_json` and `secrets_json` are kind-specific;
/// the store layer keeps them as opaque JSON and only the
/// `roy-mcp serve-connections` consumer parses them.
///
/// Wire shape. Row decoding happens manually in `Store` because workspace
/// sqlx does not enable the `json` feature, so `serde_json::Value` has no
/// `Decode<Sqlite>` impl. `Store::list/get/...` deserialize the `*_json`
/// TEXT columns into `Value` explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub slug: String,
    pub kind: String,
    pub config: Value,
    pub secrets: Option<Value>,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub provider_id: Option<String>,
}

/// Two ways to create a connection:
/// * **Catalog-backed:** `{ provider_id, name, secrets }` — backend resolves
///   command/args/env from the yaml catalog. The dominant flow.
/// * **Legacy/custom:** `{ name, kind, config, secrets }` — free-form.
///   Kept for the existing CLI/test paths; UI no longer exposes it in MVP.
///
/// `serde(untagged)` picks the right variant by which fields the body has.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NewConnection {
    FromProvider(NewConnectionFromProvider),
    Custom(NewConnectionCustom),
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewConnectionFromProvider {
    pub provider_id: String,
    pub name: String,
    #[serde(default)]
    pub secrets: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewConnectionCustom {
    pub name: String,
    pub kind: String,
    pub config: Value,
    #[serde(default)]
    pub secrets: Option<Value>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConnectionUpdate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default, deserialize_with = "crate::http::deserialize_optional_field")]
    pub secrets: Option<Option<Value>>,
    #[serde(default, deserialize_with = "crate::http::deserialize_optional_field")]
    pub description: Option<Option<String>>,
}

pub const KIND_MCP_STDIO: &str = "mcp_stdio";

/// Reject unsupported kinds. MVP supports only `mcp_stdio`.
pub fn validate_kind(kind: &str) -> Result<(), String> {
    match kind {
        KIND_MCP_STDIO => Ok(()),
        other => Err(format!(
            "unsupported connection kind '{other}'; MVP supports only 'mcp_stdio'"
        )),
    }
}

/// Validate `config_json` shape for a given `kind`. Returns a human-readable
/// reason on failure (mapped to HTTP 400 by the handler layer).
pub fn validate_config(kind: &str, config: &Value) -> Result<(), String> {
    match kind {
        KIND_MCP_STDIO => {
            let obj = config
                .as_object()
                .ok_or_else(|| "config must be an object".to_string())?;
            let cmd = obj
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| "config.command (string) is required".to_string())?;
            if cmd.is_empty() {
                return Err("config.command must be non-empty".to_string());
            }
            if let Some(args) = obj.get("args") {
                if !args.is_array() {
                    return Err("config.args must be an array of strings".to_string());
                }
                for (i, a) in args.as_array().unwrap().iter().enumerate() {
                    if !a.is_string() {
                        return Err(format!("config.args[{i}] must be a string"));
                    }
                }
            }
            if let Some(env) = obj.get("env") {
                if !env.is_object() {
                    return Err("config.env must be an object {KEY: value-string}".to_string());
                }
                for (k, v) in env.as_object().unwrap() {
                    if !v.is_string() {
                        return Err(format!("config.env[{k}] must be a string"));
                    }
                }
            }
            Ok(())
        }
        _ => Err(format!("validation not implemented for kind '{kind}'")),
    }
}

/// Lowercase, non-alphanumeric runs collapse to a single `-`, leading/trailing
/// `-` trimmed. Empty input (or all-punctuation) yields `"connection"`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "connection".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------- Store ----------------

use sqlx::SqlitePool;
use uuid::Uuid;

/// Row tuple for `connections` SELECTs — kept in one place so the three
/// callsites (`list_by_owner`, `get`, `row_to_connection`) stay aligned.
type ConnectionRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<String>,
);

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("connection not found: {0}")]
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

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a row built directly from a catalog provider definition.
    pub async fn create_from_provider(
        &self,
        owner_id: &str,
        req: NewConnectionFromProvider,
        provider: &crate::provider_catalog::Provider,
    ) -> Result<Connection, StoreError> {
        validate_required_secrets(provider, req.secrets.as_ref()).map_err(StoreError::Invalid)?;
        let config = serde_json::json!({
            "command": provider.command,
            "args": provider.args,
            "env": provider.env,
        });
        let custom = NewConnectionCustom {
            name: req.name,
            kind: KIND_MCP_STDIO.to_string(),
            config,
            secrets: req.secrets,
            description: Some(provider.description.clone()).filter(|s| !s.is_empty()),
        };
        self.create_inner(owner_id, custom, Some(provider.id.clone()))
            .await
    }

    /// Free-form CRUD path — used by the legacy CLI/test surface. UI no
    /// longer exposes this.
    pub async fn create_custom(
        &self,
        owner_id: &str,
        req: NewConnectionCustom,
    ) -> Result<Connection, StoreError> {
        self.create_inner(owner_id, req, None).await
    }

    async fn create_inner(
        &self,
        owner_id: &str,
        new: NewConnectionCustom,
        provider_id: Option<String>,
    ) -> Result<Connection, StoreError> {
        validate_kind(&new.kind).map_err(StoreError::Invalid)?;
        validate_config(&new.kind, &new.config).map_err(StoreError::Invalid)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let base = slugify(&new.name);
        let cfg_text = serialize_json("config", &new.config)?;
        let secrets_text = new
            .secrets
            .as_ref()
            .map(|v| serialize_json("secrets", v))
            .transpose()?;
        loop {
            let slug = self.unique_slug(owner_id, &base).await?;
            let res = sqlx::query(
                "INSERT INTO connections
                 (id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at, provider_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(owner_id)
            .bind(&new.name)
            .bind(&slug)
            .bind(&new.kind)
            .bind(&cfg_text)
            .bind(secrets_text.as_deref())
            .bind(new.description.as_deref())
            .bind(now)
            .bind(now)
            .bind(provider_id.as_deref())
            .execute(&self.pool)
            .await;
            match res {
                Ok(_) => {
                    return Ok(Connection {
                        id,
                        owner_id: owner_id.to_string(),
                        name: new.name,
                        slug,
                        kind: new.kind,
                        config: new.config,
                        secrets: new.secrets,
                        description: new.description,
                        created_at: now,
                        updated_at: now,
                        provider_id,
                    });
                }
                Err(sqlx::Error::Database(d)) if d.is_unique_violation() => {
                    // Only retry on slug collisions. Other UNIQUE violations
                    // (e.g. the partial `(owner_id, provider_id, name)` index
                    // for catalog-backed rows) must propagate so the handler
                    // can map them to 409. Without this guard the slug-retry
                    // loop would spin forever — the regenerated slug doesn't
                    // affect a `(provider_id, name)` collision.
                    //
                    // We match on the literal `"connections.slug"` substring
                    // because sqlx-sqlite 0.8 does NOT populate
                    // `DatabaseError::constraint()` (Postgres-only). SQLite's
                    // UNIQUE error message format is stable:
                    // `UNIQUE constraint failed: <table>.<col>, ...`.
                    if d.message().contains("connections.slug") {
                        continue;
                    }
                    return Err(StoreError::Db(sqlx::Error::Database(d)));
                }
                Err(e) => return Err(StoreError::Db(e)),
            }
        }
    }

    pub async fn list_by_owner(&self, owner_id: &str) -> Result<Vec<Connection>, StoreError> {
        let rows: Vec<ConnectionRow> = sqlx::query_as(
            "SELECT id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at, provider_id
             FROM connections WHERE owner_id = ? ORDER BY created_at DESC",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_connection).collect()
    }

    pub async fn get(&self, owner_id: &str, id: &str) -> Result<Connection, StoreError> {
        let row: Option<ConnectionRow> = sqlx::query_as(
            "SELECT id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at, provider_id
             FROM connections WHERE owner_id = ? AND id = ?",
        )
        .bind(owner_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.ok_or_else(|| StoreError::NotFound(id.to_string()))
            .and_then(row_to_connection)
    }

    /// Resolve a batch of ids belonging to `owner_id`. Unknown ids produce
    /// `StoreError::NotFound` with the first missing id.
    pub async fn get_many(
        &self,
        owner_id: &str,
        ids: &[String],
    ) -> Result<Vec<Connection>, StoreError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(self.get(owner_id, id).await?);
        }
        Ok(out)
    }

    pub async fn update(
        &self,
        owner_id: &str,
        id: &str,
        upd: ConnectionUpdate,
    ) -> Result<Connection, StoreError> {
        let current = self.get(owner_id, id).await?;
        let name = upd.name.unwrap_or(current.name);
        let config = upd.config.unwrap_or(current.config);
        validate_config(&current.kind, &config).map_err(StoreError::Invalid)?;
        let secrets = match upd.secrets {
            Some(Some(v)) => Some(v),
            Some(None) => None,
            None => current.secrets,
        };
        let description = match upd.description {
            Some(Some(s)) => Some(s),
            Some(None) => None,
            None => current.description,
        };
        let now = Utc::now().timestamp();
        let cfg_text = serialize_json("config", &config)?;
        let secrets_text = secrets
            .as_ref()
            .map(|v| serialize_json("secrets", v))
            .transpose()?;
        sqlx::query(
            "UPDATE connections SET name = ?, config_json = ?, secrets_json = ?, description = ?, updated_at = ?
             WHERE owner_id = ? AND id = ?",
        )
        .bind(&name)
        .bind(&cfg_text)
        .bind(secrets_text.as_deref())
        .bind(description.as_deref())
        .bind(now)
        .bind(owner_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(Connection {
            updated_at: now,
            name,
            config,
            secrets,
            description,
            ..current
        })
    }

    pub async fn delete(&self, owner_id: &str, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM connections WHERE owner_id = ? AND id = ?")
            .bind(owner_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn unique_slug(&self, owner_id: &str, base: &str) -> Result<String, StoreError> {
        let mut candidate = base.to_string();
        let mut n = 2;
        loop {
            let exists: Option<(i64,)> =
                sqlx::query_as("SELECT 1 FROM connections WHERE owner_id = ? AND slug = ? LIMIT 1")
                    .bind(owner_id)
                    .bind(&candidate)
                    .fetch_optional(&self.pool)
                    .await?;
            if exists.is_none() {
                return Ok(candidate);
            }
            candidate = format!("{base}-{n}");
            n += 1;
        }
    }
}

fn serialize_json(field: &str, v: &Value) -> Result<String, StoreError> {
    serde_json::to_string(v)
        .map_err(|e| StoreError::Invalid(format!("{field} not serializable: {e}")))
}

fn row_to_connection(r: ConnectionRow) -> Result<Connection, StoreError> {
    let (
        id,
        owner_id,
        name,
        slug,
        kind,
        config_json,
        secrets_json,
        description,
        created_at,
        updated_at,
        provider_id,
    ) = r;
    let config: Value = serde_json::from_str(&config_json)
        .map_err(|e| StoreError::Invalid(format!("config_json corrupt: {e}")))?;
    let secrets = match secrets_json {
        Some(s) => Some(
            serde_json::from_str::<Value>(&s)
                .map_err(|e| StoreError::Invalid(format!("secrets_json corrupt: {e}")))?,
        ),
        None => None,
    };
    Ok(Connection {
        id,
        owner_id,
        name,
        slug,
        kind,
        config,
        secrets,
        description,
        created_at,
        updated_at,
        provider_id,
    })
}

fn validate_required_secrets(
    provider: &crate::provider_catalog::Provider,
    supplied: Option<&Value>,
) -> Result<(), String> {
    if provider.secrets.is_empty() {
        return Ok(());
    }
    let supplied_obj = supplied.and_then(Value::as_object).ok_or_else(|| {
        format!(
            "secrets must be an object with keys: {}",
            required_keys(provider)
        )
    })?;
    let mut missing: Vec<&str> = Vec::new();
    for s in &provider.secrets {
        match supplied_obj.get(&s.key) {
            Some(Value::String(v)) if !v.is_empty() => {}
            _ => missing.push(s.key.as_str()),
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing required secrets: {}", missing.join(", ")))
    }
}

fn required_keys(provider: &crate::provider_catalog::Provider) -> String {
    provider
        .secrets
        .iter()
        .map(|s| s.key.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------- HTTP ----------------

use axum::{
    extract::{Extension, Path as AxPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use crate::auth::AuthUser;
use crate::state::AppState;
use serde_json::json;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(id) => {
                ApiError(StatusCode::NOT_FOUND, format!("connection not found: {id}"))
            }
            StoreError::Invalid(msg) => ApiError(StatusCode::BAD_REQUEST, msg),
            StoreError::Db(e) => {
                tracing::error!(error = %e, "connection store db error");
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connections", get(list_handler).post(create_handler))
        .route(
            "/connections/{id}",
            get(get_handler).put(update_handler).delete(delete_handler),
        )
        .route("/providers", get(providers_handler))
}

async fn list_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
) -> Result<Json<Vec<Connection>>, ApiError> {
    Ok(Json(s.connections.list_by_owner(&uid).await?))
}

async fn create_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    Json(body): Json<NewConnection>,
) -> Result<(StatusCode, Json<Connection>), ApiError> {
    let c = match body {
        NewConnection::FromProvider(req) => {
            let provider = s
                .catalog
                .get(&req.provider_id)
                .ok_or_else(|| {
                    ApiError(
                        StatusCode::BAD_REQUEST,
                        format!("unknown provider: {}", req.provider_id),
                    )
                })?
                .clone();
            s.connections
                .create_from_provider(&uid, req, &provider)
                .await
                .map_err(map_store_err)?
        }
        NewConnection::Custom(req) => s
            .connections
            .create_custom(&uid, req)
            .await
            .map_err(map_store_err)?,
    };
    Ok((StatusCode::CREATED, Json(c)))
}

fn map_store_err(e: StoreError) -> ApiError {
    // UNIQUE violation on the partial index `connections_owner_provider_label_unique`
    // → 409 Conflict with a user-readable message.
    if let StoreError::Db(sqlx::Error::Database(d)) = &e {
        if d.is_unique_violation() {
            return ApiError(
                StatusCode::CONFLICT,
                "a connection with this provider and label already exists".into(),
            );
        }
    }
    e.into()
}

async fn get_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Json<Connection>, ApiError> {
    Ok(Json(s.connections.get(&uid, &id).await?))
}

async fn update_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(upd): Json<ConnectionUpdate>,
) -> Result<Json<Connection>, ApiError> {
    Ok(Json(s.connections.update(&uid, &id, upd).await?))
}

async fn delete_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<StatusCode, ApiError> {
    s.connections.delete(&uid, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn providers_handler(
    Extension(_): Extension<AuthUser>,
    State(s): State<AppState>,
) -> Json<Vec<crate::provider_catalog::Provider>> {
    Json(s.catalog.providers().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy_auth::test_support::make_user;
    use serde_json::json;
    use sqlx::SqlitePool;

    // ----- existing validator tests (keep as-is) -----

    #[test]
    fn rejects_unknown_kind() {
        assert!(validate_kind("nango").is_err());
        assert!(validate_kind("mcp_http").is_err());
        assert!(validate_kind(KIND_MCP_STDIO).is_ok());
    }

    #[test]
    fn rejects_missing_command() {
        let err = validate_config(KIND_MCP_STDIO, &json!({})).unwrap_err();
        assert!(err.contains("command"), "{err}");
    }

    #[test]
    fn accepts_minimal_stdio() {
        validate_config(KIND_MCP_STDIO, &json!({"command": "npx"})).unwrap();
    }

    #[test]
    fn rejects_non_string_env() {
        let err =
            validate_config(KIND_MCP_STDIO, &json!({"command": "x", "env": {"K": 1}})).unwrap_err();
        assert!(err.contains("env"), "{err}");
    }

    // ----- store fixture -----

    /// Returns a pool with management + auth migrations applied, the canonical
    /// pattern used by `tests/common/mod.rs`.
    async fn setup_pool() -> SqlitePool {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        roy_auth::apply_migrations(&pool).await.unwrap();
        // Keep the tempdir alive for the test's lifetime — dropping it would
        // invalidate the SQLite file referenced by the pool.
        std::mem::forget(dir);
        pool
    }

    // ----- Store tests -----

    #[tokio::test]
    async fn create_list_get_update_delete() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let store = Store::new(pool.clone());

        let c = store
            .create_custom(
                &user.id,
                NewConnectionCustom {
                    name: "My Linear".into(),
                    kind: KIND_MCP_STDIO.into(),
                    config: json!({"command": "npx", "args": ["-y", "@linear/mcp"]}),
                    secrets: Some(json!({"LINEAR_API_KEY": "lin_xxx"})),
                    description: Some("work".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(c.slug, "my-linear");

        let listed = store.list_by_owner(&user.id).await.unwrap();
        assert_eq!(listed.len(), 1);

        let got = store.get(&user.id, &c.id).await.unwrap();
        assert_eq!(got.id, c.id);

        let upd = store
            .update(
                &user.id,
                &c.id,
                ConnectionUpdate {
                    description: Some(Some("personal".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(upd.description.as_deref(), Some("personal"));

        store.delete(&user.id, &c.id).await.unwrap();
        assert!(store.list_by_owner(&user.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn slug_collisions_get_suffixed() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let store = Store::new(pool.clone());
        let a = store
            .create_custom(
                &user.id,
                NewConnectionCustom {
                    name: "Linear".into(),
                    kind: KIND_MCP_STDIO.into(),
                    config: json!({"command": "npx"}),
                    secrets: None,
                    description: None,
                },
            )
            .await
            .unwrap();
        let b = store
            .create_custom(
                &user.id,
                NewConnectionCustom {
                    name: "Linear".into(),
                    kind: KIND_MCP_STDIO.into(),
                    config: json!({"command": "npx"}),
                    secrets: None,
                    description: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(a.slug, "linear");
        assert_eq!(b.slug, "linear-2");
    }

    #[tokio::test]
    async fn one_owner_cannot_see_another_users_connections() {
        let pool = setup_pool().await;
        let alice = make_user(&pool, "alice").await;
        let bob = make_user(&pool, "bob").await;
        let store = Store::new(pool.clone());
        store
            .create_custom(
                &alice.id,
                NewConnectionCustom {
                    name: "L".into(),
                    kind: KIND_MCP_STDIO.into(),
                    config: json!({"command": "npx"}),
                    secrets: None,
                    description: None,
                },
            )
            .await
            .unwrap();
        assert!(store.list_by_owner(&bob.id).await.unwrap().is_empty());
    }
}
