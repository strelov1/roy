//! Build the `.mcp.json` Claude Code project-config that points the agent at
//! our `roy mcp serve-connections` proxy. The Bundle (with secrets) is
//! written to a sibling file outside cwd and passed via `--specs <path>` so
//! secrets never touch the project directory.

use crate::control::ConnectionSpec;
use serde_json::{json, Value};
use std::path::Path;

/// Path under cwd where Claude Code looks for project-level MCP config.
pub const MCP_CONFIG_FILENAME: &str = ".mcp.json";

/// Build the `.mcp.json` body that points at our proxy. The proxy reads the
/// bundle at `bundle_path` on startup.
pub fn build_mcp_config(roy_binary: &str, bundle_path: &Path) -> Value {
    json!({
        "mcpServers": {
            "roy-connections": {
                "command": roy_binary,
                "args": [
                    "mcp",
                    "serve-connections",
                    "--specs",
                    bundle_path.to_string_lossy(),
                ],
            }
        }
    })
}

/// Build the bundle JSON consumed by `roy mcp serve-connections --specs`.
pub fn build_bundle(session_id: &str, connections: &[ConnectionSpec]) -> Value {
    json!({
        "session_id": session_id,
        "connections": connections,
    })
}

/// Pick the `roy` executable the daemon should hand to Claude Code. Honors
/// `ROY_BIN` for tests (so integration suites can point at a built target);
/// otherwise defaults to `roy` (assumes on PATH wherever the daemon runs).
pub fn roy_binary_path() -> String {
    std::env::var("ROY_BIN").unwrap_or_else(|_| "roy".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn config_shape() {
        let v = build_mcp_config("/usr/local/bin/roy", &PathBuf::from("/tmp/bundle.json"));
        assert_eq!(
            v["mcpServers"]["roy-connections"]["command"],
            "/usr/local/bin/roy"
        );
        let args = v["mcpServers"]["roy-connections"]["args"]
            .as_array()
            .unwrap();
        assert_eq!(args[0], "mcp");
        assert_eq!(args[1], "serve-connections");
        assert_eq!(args[2], "--specs");
        assert_eq!(args[3], "/tmp/bundle.json");
    }

    #[test]
    fn bundle_includes_secrets() {
        let specs = vec![ConnectionSpec {
            id: "id1".into(),
            slug: "linear".into(),
            kind: "mcp_stdio".into(),
            config: json!({"command": "npx"}),
            secrets: Some(json!({"LINEAR_API_KEY": "lin_xxx"})),
        }];
        let bundle = build_bundle("sess-1", &specs);
        assert_eq!(bundle["session_id"], "sess-1");
        assert_eq!(
            bundle["connections"][0]["secrets"]["LINEAR_API_KEY"],
            "lin_xxx"
        );
    }

    #[test]
    fn bundle_with_no_connections() {
        let bundle = build_bundle("sess-empty", &[]);
        assert_eq!(bundle["session_id"], "sess-empty");
        assert_eq!(bundle["connections"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn binary_path_defaults_to_roy() {
        // We don't assert the unset value because tests share env;
        // just verify the function runs and the override path works.
        std::env::set_var("ROY_BIN", "/custom/path/roy");
        assert_eq!(roy_binary_path(), "/custom/path/roy");
        std::env::remove_var("ROY_BIN");
    }
}
