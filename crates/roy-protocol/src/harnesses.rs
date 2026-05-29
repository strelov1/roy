use serde::{Deserialize, Serialize};

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
