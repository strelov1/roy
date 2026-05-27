//! User-owned MCP connections: types, store, and HTTP handlers.
//!
//! Owner is always a user (no team-shared connections in MVP). Slugs are
//! derived from `name` and made unique per-owner by suffixing (`-2`, `-3`,
//! ...) — same pattern as `roy_agents::store`.

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
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewConnection {
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
    #[serde(
        default,
        deserialize_with = "roy_agents::types::deserialize_optional_field"
    )]
    pub secrets: Option<Option<Value>>,
    #[serde(
        default,
        deserialize_with = "roy_agents::types::deserialize_optional_field"
    )]
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

/// Slugify the connection name using the same rules as roy_agents.
pub fn slugify(name: &str) -> String {
    roy_agents::slugify(name)
}

// ---------------- Store ----------------

use sqlx::SqlitePool;
use uuid::Uuid;

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

    /// Insert a new connection for `owner_id`. The slug is derived from `name`
    /// and made unique per-owner by suffixing (`base-2`, `base-3`, ...).
    pub async fn create(
        &self,
        owner_id: &str,
        new: NewConnection,
    ) -> Result<Connection, StoreError> {
        validate_kind(&new.kind).map_err(StoreError::Invalid)?;
        validate_config(&new.kind, &new.config).map_err(StoreError::Invalid)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let base = slugify(&new.name);
        loop {
            let slug = self.unique_slug(owner_id, &base).await?;
            let cfg_text = serde_json::to_string(&new.config)
                .map_err(|e| StoreError::Invalid(format!("config not serializable: {e}")))?;
            let secrets_text =
                match &new.secrets {
                    Some(v) => Some(serde_json::to_string(v).map_err(|e| {
                        StoreError::Invalid(format!("secrets not serializable: {e}"))
                    })?),
                    None => None,
                };
            let res = sqlx::query(
                "INSERT INTO connections
                 (id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                    });
                }
                Err(sqlx::Error::Database(d)) if d.is_unique_violation() => continue,
                Err(e) => return Err(StoreError::Db(e)),
            }
        }
    }

    pub async fn list_by_owner(&self, owner_id: &str) -> Result<Vec<Connection>, StoreError> {
        let rows: Vec<(
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
        )> = sqlx::query_as(
            "SELECT id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at
             FROM connections WHERE owner_id = ? ORDER BY created_at DESC",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_connection).collect()
    }

    pub async fn get(&self, owner_id: &str, id: &str) -> Result<Connection, StoreError> {
        let row: Option<(
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
        )> = sqlx::query_as(
            "SELECT id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at
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
        let name = upd.name.clone().unwrap_or(current.name.clone());
        let config = upd.config.clone().unwrap_or(current.config.clone());
        validate_config(&current.kind, &config).map_err(StoreError::Invalid)?;
        let secrets = match upd.secrets {
            Some(Some(v)) => Some(v),
            Some(None) => None,
            None => current.secrets.clone(),
        };
        let description = match upd.description {
            Some(Some(s)) => Some(s),
            Some(None) => None,
            None => current.description.clone(),
        };
        let now = Utc::now().timestamp();
        let cfg_text = serde_json::to_string(&config)
            .map_err(|e| StoreError::Invalid(format!("config not serializable: {e}")))?;
        let secrets_text = match &secrets {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| StoreError::Invalid(format!("secrets not serializable: {e}")))?,
            ),
            None => None,
        };
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

fn row_to_connection(
    r: (
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
    ),
) -> Result<Connection, StoreError> {
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
    })
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

    /// Returns a pool with all migrations applied (roy-agents + roy-management
    /// + roy-auth), the canonical pattern used by `tests/common/mod.rs`.
    /// `roy_auth::test_support::temp_pool` alone only applies roy-auth
    /// migrations and skips the roy-agents `agents` table that
    /// `MetaStore::apply_migrations` references via FK / migration shape, so
    /// we open via `roy_agents::open` first.
    async fn setup_pool() -> SqlitePool {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        crate::meta_store::MetaStore::apply_migrations(&pool)
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
            .create(
                &user.id,
                NewConnection {
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
            .create(
                &user.id,
                NewConnection {
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
            .create(
                &user.id,
                NewConnection {
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
            .create(
                &alice.id,
                NewConnection {
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
