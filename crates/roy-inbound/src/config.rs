//! TOML config for roy-inbound. One global `[bus]`, one `[server]`, and
//! N `[[sources]]` blocks. Each source declares a `kind` and a matching
//! `[sources.<kind>]` sub-table.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::channels::webhook::config::WebhookConfig;
use crate::session::SessionStrategyConfig;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundConfig {
    #[serde(default)]
    pub bus: BusConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusConfig {
    #[serde(default = "default_capacity")]
    pub capacity: usize,
}
impl Default for BusConfig {
    fn default() -> Self {
        Self { capacity: default_capacity() }
    }
}
fn default_capacity() -> usize {
    256
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self { bind: default_bind() }
    }
}
fn default_bind() -> String {
    "127.0.0.1:8090".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub id: String,
    pub kind: String,
    pub agent_id: String,
    pub session: SessionStrategyConfig,
    #[serde(default = "default_fire_timeout")]
    pub fire_timeout_secs: u64,
    pub template: String,
    pub webhook: Option<WebhookConfig>,
}
fn default_fire_timeout() -> u64 {
    600
}

impl InboundConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for src in &self.sources {
            if !ids.insert(src.id.clone()) {
                return Err(anyhow!("duplicate source id: {}", src.id));
            }
            match src.kind.as_str() {
                "webhook" => {
                    let wh = src.webhook.as_ref().ok_or_else(|| {
                        anyhow!("source {}: missing [sources.webhook]", src.id)
                    })?;
                    if !paths.insert(wh.path.clone()) {
                        return Err(anyhow!("duplicate webhook path: {}", wh.path));
                    }
                }
                other => return Err(anyhow!("source {}: unknown kind '{}'", src.id, other)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_and_load(content: &str) -> Result<InboundConfig> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbound.toml");
        std::fs::write(&path, content).unwrap();
        InboundConfig::load(&path)
    }

    #[test]
    fn minimal_webhook_loads() {
        let cfg = write_and_load(
            r#"
            [[sources]]
            id = "orders"
            kind = "webhook"
            agent_id = "order-bot"
            session = "ephemeral"
            template = "New: {{payload.body}}"
            [sources.webhook]
            path = "/orders"
            reply_mode = "sync"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.sources[0].fire_timeout_secs, 600);
        assert_eq!(cfg.bus.capacity, 256);
    }

    #[test]
    fn duplicate_source_id_rejected() {
        let err = write_and_load(
            r#"
            [[sources]]
            id = "x"
            kind = "webhook"
            agent_id = "a"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/a"
            reply_mode = "sync"
            [[sources]]
            id = "x"
            kind = "webhook"
            agent_id = "a"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/b"
            reply_mode = "sync"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate source id"));
    }

    #[test]
    fn duplicate_webhook_path_rejected() {
        let err = write_and_load(
            r#"
            [[sources]]
            id = "a"
            kind = "webhook"
            agent_id = "x"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/shared"
            reply_mode = "sync"
            [[sources]]
            id = "b"
            kind = "webhook"
            agent_id = "x"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/shared"
            reply_mode = "sync"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate webhook path"));
    }

    #[test]
    fn unknown_kind_rejected() {
        let err = write_and_load(
            r#"
            [[sources]]
            id = "x"
            kind = "carrier-pigeon"
            agent_id = "a"
            session = "ephemeral"
            template = "t"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown kind"));
    }
}
