//! User-owned MCP connections: types, store, and HTTP handlers.
//!
//! Owner is always a user (no team-shared connections in MVP). Slugs are
//! derived from `name` and made unique per-owner by suffixing (`-2`, `-3`,
//! ...) — same pattern as `roy_agents::store`.

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
