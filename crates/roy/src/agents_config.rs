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
    pub fn validate(&self) -> Result<(), AgentsConfigError> {
        use std::collections::HashSet;

        let mut seen_preset = HashSet::new();
        for agent in &self.agents {
            if !seen_preset.insert(agent.preset) {
                return Err(AgentsConfigError::Validate(format!(
                    "duplicate preset '{}'",
                    agent.preset
                )));
            }

            let defaults: Vec<&str> = agent
                .models
                .iter()
                .filter(|m| m.default)
                .map(|m| m.id.as_str())
                .collect();
            if defaults.len() > 1 {
                return Err(AgentsConfigError::Validate(format!(
                    "agent '{}': two models marked default ({})",
                    agent.preset,
                    defaults.join(", ")
                )));
            }

            let mut seen_id = HashSet::new();
            for m in &agent.models {
                if m.id.trim().is_empty() {
                    return Err(AgentsConfigError::Validate(format!(
                        "agent '{}': empty model id",
                        agent.preset
                    )));
                }
                if !seen_id.insert(m.id.as_str()) {
                    return Err(AgentsConfigError::Validate(format!(
                        "agent '{}': duplicate model id '{}'",
                        agent.preset, m.id
                    )));
                }
                if let Some(label) = &m.label {
                    if label.trim().is_empty() {
                        return Err(AgentsConfigError::Validate(format!(
                            "agent '{}' model '{}': empty label",
                            agent.preset, m.id
                        )));
                    }
                }
            }
        }
        Ok(())
    }

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

    #[test]
    fn rejects_duplicate_preset() {
        let text = r#"
        [[agent]]
        preset = "claude"

        [[agent]]
        preset = "claude"
    "#;
        let cfg = AgentsConfig::parse(text).unwrap();
        let err = cfg.validate().unwrap_err();
        let AgentsConfigError::Validate(msg) = err else {
            panic!("wrong variant")
        };
        assert!(msg.contains("duplicate"), "got: {msg}");
        assert!(msg.contains("claude"), "got: {msg}");
    }

    #[test]
    fn rejects_two_defaults_in_one_agent() {
        let text = r#"
        [[agent]]
        preset = "claude"
        [[agent.models]]
        id = "claude-sonnet-4-6"
        default = true
        [[agent.models]]
        id = "claude-opus-4-7"
        default = true
    "#;
        let cfg = AgentsConfig::parse(text).unwrap();
        let err = cfg.validate().unwrap_err();
        let AgentsConfigError::Validate(msg) = err else {
            panic!("wrong variant")
        };
        assert!(msg.contains("claude"));
        assert!(msg.contains("claude-sonnet-4-6") && msg.contains("claude-opus-4-7"));
    }

    #[test]
    fn rejects_duplicate_model_id_in_one_agent() {
        let text = r#"
        [[agent]]
        preset = "claude"
        [[agent.models]]
        id = "x"
        [[agent.models]]
        id = "x"
    "#;
        let cfg = AgentsConfig::parse(text).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, AgentsConfigError::Validate(_)));
    }

    #[test]
    fn rejects_empty_id() {
        let text = r#"
        [[agent]]
        preset = "claude"
        [[agent.models]]
        id = ""
    "#;
        let cfg = AgentsConfig::parse(text).unwrap();
        assert!(matches!(
            cfg.validate(),
            Err(AgentsConfigError::Validate(_))
        ));
    }
}
