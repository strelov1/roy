//! User-owned configuration for which ACP presets are available and which
//! models to surface per preset. Source of truth is a TOML file at
//! `~/.config/roy/agents.toml`. This module owns parsing, validation, and
//! the bootstrap-when-missing dance.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SAMPLE_TOML: &str = include_str!("../templates/agents_sample.toml");

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

/// The hardcoded ACP presets. This is the single source of truth for the set
/// of supported agents; `daemon.rs::DefaultTransportFactory::build` matches
/// on this enum, not on a string. Adding a preset means: extend `ALL` and
/// `as_str` — `Display`, `FromStr`, the clap value_parser in `roy-cli`, and
/// the MCP JSON-schema enum in `roy-mcp` all derive from those.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentPreset {
    Claude,
    Gemini,
    Opencode,
    Codex,
    Pi,
}

impl AgentPreset {
    pub const ALL: &'static [AgentPreset] = &[
        AgentPreset::Claude,
        AgentPreset::Gemini,
        AgentPreset::Opencode,
        AgentPreset::Codex,
        AgentPreset::Pi,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AgentPreset::Claude => "claude",
            AgentPreset::Gemini => "gemini",
            AgentPreset::Opencode => "opencode",
            AgentPreset::Codex => "codex",
            AgentPreset::Pi => "pi",
        }
    }
}

impl std::fmt::Display for AgentPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentPreset {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgentPreset::ALL
            .iter()
            .copied()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| format!("unknown agent: {s}"))
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

    /// Parse + validate. The two are coupled deliberately: every caller wants
    /// a config that's both syntactically and semantically valid, so the
    /// public entry point enforces both.
    pub fn parse(text: &str) -> Result<Self, AgentsConfigError> {
        let cfg: AgentsConfig = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }
}

/// Outcome of `load_or_bootstrap`. `Created` signals the file was missing
/// and a sample was written; callers expose this as `status: created` on
/// the wire so the UI can show a one-time hint.
#[derive(Debug)]
pub enum LoadOutcome {
    Ok(AgentsConfig),
    Created,
}

/// Resolve the config path. Precedence:
/// 1. `$ROY_AGENTS_CONFIG` (override; mostly for tests + systemd).
/// 2. `$XDG_CONFIG_HOME/roy/agents.toml`.
/// 3. `$HOME/.config/roy/agents.toml`.
///
/// Returns an error only if `$HOME` is unset *and* the fallback is needed.
pub fn config_path() -> Result<PathBuf, AgentsConfigError> {
    if let Ok(p) = std::env::var("ROY_AGENTS_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("roy").join("agents.toml"));
        }
    }
    let home = std::env::var("HOME").map_err(|_| {
        AgentsConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "HOME unset, cannot locate agents.toml",
        ))
    })?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("roy")
        .join("agents.toml"))
}

/// Atomic write: unique temp file + rename. Crash-safe; concurrent callers
/// race on `rename` and the loser silently overwrites with identical content.
/// Each call uses a unique temp filename so concurrent writers don't clobber
/// each other's in-flight temp file.
async fn write_sample(path: &Path) -> Result<(), AgentsConfigError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("agents.toml"),
        uuid::Uuid::new_v4().as_simple(),
    ));
    tokio::fs::write(&tmp, SAMPLE_TOML).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Wire-facing per-model record. `label` is always filled (daemon fills
/// in `id` when the user omitted it); `default` is always non-`false`
/// for exactly one model per agent (daemon promotes the first if needed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    pub default: bool,
}

/// Wire-facing per-agent record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInfo {
    pub preset: AgentPreset,
    pub models: Vec<ModelInfo>,
}

/// Status field on the `AgentsList` event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentsConfigStatus {
    Ok,
    Created,
    Invalid { reason: String },
}

impl AgentsConfig {
    /// Convert to the wire shape, applying daemon-side normalisation:
    /// fill `label = id` when omitted; promote the first model to default
    /// if none was set explicitly.
    pub fn into_wire(self) -> Vec<AgentInfo> {
        self.agents
            .into_iter()
            .map(|a| {
                let any_default = a.models.iter().any(|m| m.default);
                let models = a
                    .models
                    .into_iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let label = m.label.unwrap_or_else(|| m.id.clone());
                        let default = m.default || (!any_default && i == 0);
                        ModelInfo {
                            id: m.id,
                            label,
                            default,
                        }
                    })
                    .collect();
                AgentInfo {
                    preset: a.preset,
                    models,
                }
            })
            .collect()
    }
}

/// Read+parse+validate the config at `path`. If the file is missing, write
/// the sample and return `Created` (with no parsed config — the sample is
/// entirely commented and would yield an empty config; we surface the
/// "first run" signal instead).
pub async fn load_or_bootstrap(path: &Path) -> Result<LoadOutcome, AgentsConfigError> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(LoadOutcome::Ok(AgentsConfig::parse(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_sample(path).await?;
            Ok(LoadOutcome::Created)
        }
        Err(e) => Err(AgentsConfigError::Io(e)),
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
        let err = AgentsConfig::parse(text).unwrap_err();
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
        let err = AgentsConfig::parse(text).unwrap_err();
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
        let err = AgentsConfig::parse(text).unwrap_err();
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
        let err = AgentsConfig::parse(text).unwrap_err();
        assert!(matches!(err, AgentsConfigError::Validate(_)));
    }

    #[test]
    fn sample_file_parses_to_empty_config() {
        let cfg = AgentsConfig::parse(SAMPLE_TOML).expect("sample parses + validates");
        assert!(cfg.agents.is_empty(), "sample must be fully commented out");
    }

    #[test]
    fn into_wire_fills_label_from_id() {
        let cfg = AgentsConfig::parse(
            r#"
            [[agent]]
            preset = "claude"
            [[agent.models]]
            id = "x"
        "#,
        )
        .unwrap();
        let wire = cfg.into_wire();
        assert_eq!(wire[0].models[0].label, "x");
    }

    #[test]
    fn into_wire_promotes_first_model_when_no_default() {
        let cfg = AgentsConfig::parse(
            r#"
            [[agent]]
            preset = "claude"
            [[agent.models]]
            id = "a"
            [[agent.models]]
            id = "b"
        "#,
        )
        .unwrap();
        let wire = cfg.into_wire();
        assert!(wire[0].models[0].default);
        assert!(!wire[0].models[1].default);
    }

    #[test]
    fn into_wire_preserves_explicit_default() {
        let cfg = AgentsConfig::parse(
            r#"
            [[agent]]
            preset = "claude"
            [[agent.models]]
            id = "a"
            [[agent.models]]
            id = "b"
            default = true
        "#,
        )
        .unwrap();
        let wire = cfg.into_wire();
        assert!(!wire[0].models[0].default);
        assert!(wire[0].models[1].default);
    }

    #[tokio::test]
    async fn bootstraps_missing_file_with_sample() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agents.toml");
        let outcome = load_or_bootstrap(&path).await.unwrap();
        assert!(matches!(outcome, LoadOutcome::Created));
        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(written, SAMPLE_TOML);
    }

    #[tokio::test]
    async fn loads_existing_valid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agents.toml");
        tokio::fs::write(
            &path,
            r#"
            [[agent]]
            preset = "gemini"
            [[agent.models]]
            id = "gemini-2.5-pro"
            default = true
        "#,
        )
        .await
        .unwrap();
        let outcome = load_or_bootstrap(&path).await.unwrap();
        let LoadOutcome::Ok(cfg) = outcome else {
            panic!("expected Ok")
        };
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.agents[0].preset, AgentPreset::Gemini);
    }

    #[tokio::test]
    async fn surfaces_validation_error_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agents.toml");
        tokio::fs::write(
            &path,
            r#"
            [[agent]]
            preset = "claude"
            [[agent]]
            preset = "claude"
        "#,
        )
        .await
        .unwrap();
        let err = load_or_bootstrap(&path).await.unwrap_err();
        assert!(matches!(err, AgentsConfigError::Validate(_)));
    }
}
