//! User-owned configuration for which ACP presets are available and which
//! models to surface per preset. Source of truth is a TOML file at
//! `~/.config/roy/agents.toml`. This module owns parsing, validation, and
//! the bootstrap-when-missing dance.

use serde::{Deserialize, Serialize};

/// Raw config-file shape. Loaded via `toml::from_str`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsConfig {
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub preset: AgentPreset,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

/// The four hardcoded ACP presets. This is the single source of truth for
/// the set of supported agents; `daemon.rs::DefaultTransportFactory::build`
/// matches on this enum, not on a string.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentPreset {
    Claude,
    Gemini,
    Opencode,
    Codex,
}

impl std::fmt::Display for AgentPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AgentPreset::Claude => "claude",
            AgentPreset::Gemini => "gemini",
            AgentPreset::Opencode => "opencode",
            AgentPreset::Codex => "codex",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentsConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("{0}")]
    Validate(String),
}

impl AgentsConfig {
    pub fn parse(text: &str) -> Result<Self, AgentsConfigError> {
        let cfg: AgentsConfig = toml::from_str(text)?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_toml_with_all_fields() {
        let text = r#"
            [[agent]]
            preset = "claude"

            [[agent.models]]
            id = "claude-sonnet-4-6"
            label = "Claude Sonnet 4.6"
            default = true

            [[agent.models]]
            id = "claude-opus-4-7"
            label = "Claude Opus 4.7"
        "#;
        let cfg = AgentsConfig::parse(text).unwrap();
        assert_eq!(cfg.agents.len(), 1);
        let a = &cfg.agents[0];
        assert_eq!(a.preset, AgentPreset::Claude);
        assert_eq!(a.models.len(), 2);
        assert_eq!(a.models[0].id, "claude-sonnet-4-6");
        assert_eq!(a.models[0].label.as_deref(), Some("Claude Sonnet 4.6"));
        assert!(a.models[0].default);
        assert!(!a.models[1].default);
    }

    #[test]
    fn parses_empty_config() {
        let cfg = AgentsConfig::parse("").unwrap();
        assert!(cfg.agents.is_empty());
    }

    #[test]
    fn rejects_unknown_preset_value() {
        let text = r#"
            [[agent]]
            preset = "klaude"
        "#;
        let err = AgentsConfig::parse(text).unwrap_err();
        assert!(matches!(err, AgentsConfigError::Parse(_)));
    }
}
