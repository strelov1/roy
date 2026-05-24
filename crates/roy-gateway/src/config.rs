//! TOML configuration loaded from a single file (typically
//! `~/.config/roy-gateway/config.toml`).

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    pub telegram: TelegramConfig,
    pub binder: BinderConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct DaemonConfig {
    /// Override for the roy daemon Unix socket. When `None`, fall back to
    /// `ROY_SOCKET` env var, then `~/.roy/daemon.sock`.
    #[serde(default)]
    pub socket: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramConfig {
    pub token: String,
    /// If empty, all users may DM the bot. Otherwise, only listed numeric
    /// Telegram user ids may.
    #[serde(default)]
    pub allowed_user_ids: Vec<u64>,
    /// roy preset to spawn for new chats (`claude` / `gemini` / `opencode` / `codex`).
    pub preset: String,
    /// Working directory for spawned sessions. `None` → daemon picks its
    /// default (`ROY_CWD` / daemon cwd).
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_turn_timeout_secs")]
    pub turn_timeout_secs: u64,
}

fn default_turn_timeout_secs() -> u64 {
    600
}

#[derive(Debug, Deserialize)]
pub struct BinderConfig {
    /// Path to the JSON file holding `chat_id → session_id`.
    pub path: String,
}

impl GatewayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let raw = r#"
            [daemon]
            socket = "/tmp/roy.sock"

            [telegram]
            token = "1234:abc"
            allowed_user_ids = [1, 2]
            preset = "claude"
            cwd = "/Users/me/proj"
            turn_timeout_secs = 300

            [binder]
            path = "/tmp/binder.json"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.daemon.socket.as_deref(), Some("/tmp/roy.sock"));
        assert_eq!(cfg.telegram.token, "1234:abc");
        assert_eq!(cfg.telegram.allowed_user_ids, vec![1, 2]);
        assert_eq!(cfg.telegram.preset, "claude");
        assert_eq!(cfg.telegram.cwd.as_deref(), Some("/Users/me/proj"));
        assert_eq!(cfg.telegram.turn_timeout_secs, 300);
        assert_eq!(cfg.binder.path, "/tmp/binder.json");
    }

    #[test]
    fn parse_minimal_config_uses_defaults() {
        let raw = r#"
            [telegram]
            token = "x"
            preset = "claude"

            [binder]
            path = "/tmp/b.json"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.daemon.socket.is_none());
        assert!(cfg.telegram.allowed_user_ids.is_empty());
        assert!(cfg.telegram.cwd.is_none());
        assert_eq!(cfg.telegram.turn_timeout_secs, 600);
    }
}
