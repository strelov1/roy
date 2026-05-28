//! User-owned configuration for which ACP harnesses are available and which
//! models to surface per harness. Source of truth is a TOML file at
//! `~/.config/roy/harnesses.toml`. This module owns parsing, validation, and
//! the bootstrap-when-missing dance.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SAMPLE_TOML: &str = include_str!("../templates/harnesses_sample.toml");

/// Raw config-file shape. Loaded via `toml::from_str`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessesConfig {
    #[serde(default, rename = "harness")]
    pub harnesses: Vec<HarnessEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessEntry {
    pub name: Harness,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

/// The hardcoded ACP harnesses. This is the single source of truth for the
/// set of supported harness binaries; `daemon.rs::DefaultTransportFactory::build`
/// matches on this enum, not on a string. Adding a harness means: extend `ALL`
/// and `as_str` — `Display`, `FromStr`, the clap value_parser in `roy-cli`,
/// and the MCP JSON-schema enum in `roy-mcp` all derive from those.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Gemini,
    Opencode,
    Codex,
    Pi,
}

impl Harness {
    pub const ALL: &'static [Harness] = &[
        Harness::Claude,
        Harness::Gemini,
        Harness::Opencode,
        Harness::Codex,
        Harness::Pi,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Gemini => "gemini",
            Harness::Opencode => "opencode",
            Harness::Codex => "codex",
            Harness::Pi => "pi",
        }
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Harness {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Harness::ALL
            .iter()
            .copied()
            .find(|h| h.as_str() == s)
            .ok_or_else(|| format!("unknown harness: {s}"))
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
pub enum HarnessesConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("{0}")]
    Validate(String),
}

impl HarnessesConfig {
    pub fn validate(&self) -> Result<(), HarnessesConfigError> {
        use std::collections::HashSet;

        let mut seen_name = HashSet::new();
        for harness in &self.harnesses {
            if !seen_name.insert(harness.name) {
                return Err(HarnessesConfigError::Validate(format!(
                    "duplicate harness '{}'",
                    harness.name
                )));
            }

            let defaults: Vec<&str> = harness
                .models
                .iter()
                .filter(|m| m.default)
                .map(|m| m.id.as_str())
                .collect();
            if defaults.len() > 1 {
                return Err(HarnessesConfigError::Validate(format!(
                    "harness '{}': two models marked default ({})",
                    harness.name,
                    defaults.join(", ")
                )));
            }

            let mut seen_id = HashSet::new();
            for m in &harness.models {
                if m.id.trim().is_empty() {
                    return Err(HarnessesConfigError::Validate(format!(
                        "harness '{}': empty model id",
                        harness.name
                    )));
                }
                if !seen_id.insert(m.id.as_str()) {
                    return Err(HarnessesConfigError::Validate(format!(
                        "harness '{}': duplicate model id '{}'",
                        harness.name, m.id
                    )));
                }
                if let Some(label) = &m.label {
                    if label.trim().is_empty() {
                        return Err(HarnessesConfigError::Validate(format!(
                            "harness '{}' model '{}': empty label",
                            harness.name, m.id
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
    pub fn parse(text: &str) -> Result<Self, HarnessesConfigError> {
        let cfg: HarnessesConfig = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }
}

/// Outcome of `load_or_bootstrap`. `Created` signals the file was missing
/// and a sample was written; callers expose this as `status: created` on
/// the wire so the UI can show a one-time hint.
#[derive(Debug)]
pub enum LoadOutcome {
    Ok(HarnessesConfig),
    Created,
}

/// Resolve the config path. Precedence:
/// 1. `$ROY_HARNESSES_CONFIG` (override; mostly for tests + systemd).
/// 2. `$XDG_CONFIG_HOME/roy/harnesses.toml`.
/// 3. `$HOME/.config/roy/harnesses.toml`.
///
/// Returns an error only if `$HOME` is unset *and* the fallback is needed.
pub fn config_path() -> Result<PathBuf, HarnessesConfigError> {
    if let Ok(p) = std::env::var("ROY_HARNESSES_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("roy").join("harnesses.toml"));
        }
    }
    let home = std::env::var("HOME").map_err(|_| {
        HarnessesConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "HOME unset, cannot locate harnesses.toml",
        ))
    })?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("roy")
        .join("harnesses.toml"))
}

/// Atomic write: unique temp file + rename. Crash-safe; concurrent callers
/// race on `rename` and the loser silently overwrites with identical content.
/// Each call uses a unique temp filename so concurrent writers don't clobber
/// each other's in-flight temp file.
async fn write_sample(path: &Path) -> Result<(), HarnessesConfigError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("harnesses.toml"),
        uuid::Uuid::new_v4().as_simple(),
    ));
    tokio::fs::write(&tmp, SAMPLE_TOML).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Wire-facing per-model record. `label` is always filled (daemon fills
/// in `id` when the user omitted it); `default` is always non-`false`
/// for exactly one model per harness (daemon promotes the first if needed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    pub default: bool,
}

/// Wire-facing per-harness record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessInfo {
    pub name: Harness,
    pub models: Vec<ModelInfo>,
}

/// Status field on the `HarnessesList` event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HarnessesConfigStatus {
    Ok,
    Created,
    Invalid { reason: String },
}

impl HarnessesConfig {
    /// Convert to the wire shape, applying daemon-side normalisation:
    /// fill `label = id` when omitted; promote the first model to default
    /// if none was set explicitly.
    pub fn into_wire(self) -> Vec<HarnessInfo> {
        self.harnesses
            .into_iter()
            .map(|h| {
                let any_default = h.models.iter().any(|m| m.default);
                let models = h
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
                HarnessInfo {
                    name: h.name,
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
pub async fn load_or_bootstrap(path: &Path) -> Result<LoadOutcome, HarnessesConfigError> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(LoadOutcome::Ok(HarnessesConfig::parse(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_sample(path).await?;
            Ok(LoadOutcome::Created)
        }
        Err(e) => Err(HarnessesConfigError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_toml_with_all_fields() {
        let text = r#"
            [[harness]]
            name = "claude"

            [[harness.models]]
            id = "claude-sonnet-4-6"
            label = "Claude Sonnet 4.6"
            default = true

            [[harness.models]]
            id = "claude-opus-4-7"
            label = "Claude Opus 4.7"
        "#;
        let cfg = HarnessesConfig::parse(text).unwrap();
        assert_eq!(cfg.harnesses.len(), 1);
        let h = &cfg.harnesses[0];
        assert_eq!(h.name, Harness::Claude);
        assert_eq!(h.models.len(), 2);
        assert_eq!(h.models[0].id, "claude-sonnet-4-6");
        assert_eq!(h.models[0].label.as_deref(), Some("Claude Sonnet 4.6"));
        assert!(h.models[0].default);
        assert!(!h.models[1].default);
    }

    #[test]
    fn parses_empty_config() {
        let cfg = HarnessesConfig::parse("").unwrap();
        assert!(cfg.harnesses.is_empty());
    }

    #[test]
    fn rejects_unknown_harness_value() {
        let text = r#"
            [[harness]]
            name = "klaude"
        "#;
        let err = HarnessesConfig::parse(text).unwrap_err();
        assert!(matches!(err, HarnessesConfigError::Parse(_)));
    }

    #[test]
    fn rejects_duplicate_harness() {
        let text = r#"
        [[harness]]
        name = "claude"

        [[harness]]
        name = "claude"
    "#;
        let err = HarnessesConfig::parse(text).unwrap_err();
        let HarnessesConfigError::Validate(msg) = err else {
            panic!("wrong variant")
        };
        assert!(msg.contains("duplicate"), "got: {msg}");
        assert!(msg.contains("claude"), "got: {msg}");
    }

    #[test]
    fn rejects_two_defaults_in_one_harness() {
        let text = r#"
        [[harness]]
        name = "claude"
        [[harness.models]]
        id = "claude-sonnet-4-6"
        default = true
        [[harness.models]]
        id = "claude-opus-4-7"
        default = true
    "#;
        let err = HarnessesConfig::parse(text).unwrap_err();
        let HarnessesConfigError::Validate(msg) = err else {
            panic!("wrong variant")
        };
        assert!(msg.contains("claude"));
        assert!(msg.contains("claude-sonnet-4-6") && msg.contains("claude-opus-4-7"));
    }

    #[test]
    fn rejects_duplicate_model_id_in_one_harness() {
        let text = r#"
        [[harness]]
        name = "claude"
        [[harness.models]]
        id = "x"
        [[harness.models]]
        id = "x"
    "#;
        let err = HarnessesConfig::parse(text).unwrap_err();
        assert!(matches!(err, HarnessesConfigError::Validate(_)));
    }

    #[test]
    fn rejects_empty_id() {
        let text = r#"
        [[harness]]
        name = "claude"
        [[harness.models]]
        id = ""
    "#;
        let err = HarnessesConfig::parse(text).unwrap_err();
        assert!(matches!(err, HarnessesConfigError::Validate(_)));
    }

    #[test]
    fn sample_file_parses_to_empty_config() {
        let cfg = HarnessesConfig::parse(SAMPLE_TOML).expect("sample parses + validates");
        assert!(
            cfg.harnesses.is_empty(),
            "sample must be fully commented out"
        );
    }

    #[test]
    fn into_wire_fills_label_from_id() {
        let cfg = HarnessesConfig::parse(
            r#"
            [[harness]]
            name = "claude"
            [[harness.models]]
            id = "x"
        "#,
        )
        .unwrap();
        let wire = cfg.into_wire();
        assert_eq!(wire[0].models[0].label, "x");
    }

    #[test]
    fn into_wire_promotes_first_model_when_no_default() {
        let cfg = HarnessesConfig::parse(
            r#"
            [[harness]]
            name = "claude"
            [[harness.models]]
            id = "a"
            [[harness.models]]
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
        let cfg = HarnessesConfig::parse(
            r#"
            [[harness]]
            name = "claude"
            [[harness.models]]
            id = "a"
            [[harness.models]]
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
        let path = tmp.path().join("harnesses.toml");
        let outcome = load_or_bootstrap(&path).await.unwrap();
        assert!(matches!(outcome, LoadOutcome::Created));
        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(written, SAMPLE_TOML);
    }

    #[tokio::test]
    async fn loads_existing_valid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("harnesses.toml");
        tokio::fs::write(
            &path,
            r#"
            [[harness]]
            name = "gemini"
            [[harness.models]]
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
        assert_eq!(cfg.harnesses.len(), 1);
        assert_eq!(cfg.harnesses[0].name, Harness::Gemini);
    }

    #[tokio::test]
    async fn surfaces_validation_error_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("harnesses.toml");
        tokio::fs::write(
            &path,
            r#"
            [[harness]]
            name = "claude"
            [[harness]]
            name = "claude"
        "#,
        )
        .await
        .unwrap();
        let err = load_or_bootstrap(&path).await.unwrap_err();
        assert!(matches!(err, HarnessesConfigError::Validate(_)));
    }
}
