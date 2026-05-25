//! TOML configuration loaded from a single file (typically
//! `~/.config/roy-gateway/config.toml`).

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub binder: Option<BinderConfig>,
    #[serde(default)]
    pub websocket: Option<WebsocketConfig>,
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
    /// Filesystem path to use as the agent's cwd when a new session is
    /// spawned for a chat. `None` lets the daemon allocate an orphan
    /// `<workspace>/<session_id>/` directory.
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
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

#[derive(Debug, Deserialize)]
pub struct WebsocketConfig {
    /// Address to bind the WS listener on. Loopback-only by default; set an
    /// external address only behind your own TLS termination.
    #[serde(default = "default_ws_bind")]
    pub bind: String,
    /// Path to the shared-secret token file. When `None`, defaults to
    /// `~/.local/state/roy-gateway/ws.token`.
    #[serde(default)]
    pub token_path: Option<String>,
}

fn default_ws_bind() -> String {
    "127.0.0.1:8787".to_string()
}

impl GatewayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// At least one adapter must be configured; Telegram needs a binder.
    pub fn validate(&self) -> Result<()> {
        if self.telegram.is_none() && self.websocket.is_none() {
            anyhow::bail!("config must enable at least one of [telegram] or [websocket]");
        }
        if self.telegram.is_some() && self.binder.is_none() {
            anyhow::bail!("[telegram] requires a [binder] section");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_telegram_and_websocket() {
        let raw = r#"
            [daemon]
            socket = "/tmp/roy.sock"

            [telegram]
            token = "1234:abc"
            preset = "claude"

            [binder]
            path = "/tmp/binder.json"

            [websocket]
            bind = "127.0.0.1:9001"
            token_path = "/tmp/ws.token"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.daemon.socket.as_deref(), Some("/tmp/roy.sock"));
        let tg = cfg.telegram.as_ref().unwrap();
        assert_eq!(tg.token, "1234:abc");
        let ws = cfg.websocket.as_ref().unwrap();
        assert_eq!(ws.bind, "127.0.0.1:9001");
        assert_eq!(ws.token_path.as_deref(), Some("/tmp/ws.token"));
    }

    #[test]
    fn websocket_only_config_parses() {
        let raw = r#"
            [websocket]
            bind = "127.0.0.1:8787"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.telegram.is_none());
        assert!(cfg.binder.is_none());
        let ws = cfg.websocket.as_ref().unwrap();
        assert_eq!(ws.bind, "127.0.0.1:8787");
        assert!(ws.token_path.is_none());
        cfg.validate().expect("ws-only is valid");
    }

    #[test]
    fn websocket_bind_defaults_when_omitted() {
        let raw = r#"
            [websocket]
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.websocket.unwrap().bind, "127.0.0.1:8787");
    }

    #[test]
    fn no_adapter_is_an_error() {
        let raw = r#"
            [daemon]
            socket = "/tmp/roy.sock"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err(), "must require at least one adapter");
    }

    #[test]
    fn telegram_without_binder_is_an_error() {
        let raw = r#"
            [telegram]
            token = "x"
            preset = "claude"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err(), "telegram requires a binder");
    }
}
