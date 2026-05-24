# Agents Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace hardcoded agent/model lists with a user-owned `~/.config/roy/agents.toml` that daemon parses on demand and exposes to CLI (`roy agents list`), MCP (`roy_list_agents`), and roy-web (replacing `model-catalog.ts`).

**Architecture:** Add a small `agents_config` module to the `roy` crate that parses and validates the TOML, with a bootstrap path that atomically writes a fully-commented sample on first run. Daemon adds one `ClientCommand::ListAgents` handler that re-reads the file on every call (no cache, no watcher). Same wire response shape feeds all three clients. The four ACP presets stay hardcoded in Rust; this is a *filter layer*, not a custom-agent registry.

**Tech Stack:** Rust 2021 (workspace `/Users/i_strelov/Projects/roy`), `tokio`, `serde`, new `toml` crate, agent-client-protocol SDK. Web client: Svelte 5 + Vite (sibling repo `/Users/i_strelov/Projects/roy-web`). `tokio::fs` for I/O. Sync `Mutex` is not needed — config is per-request stateless.

**Reference spec:** `docs/superpowers/specs/2026-05-24-agents-config-design.md`.

---

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `crates/roy/src/agents_config.rs` | Types (`AgentsConfig`, `AgentEntry`, `AgentPreset`, `ModelEntry`); `validate()`; `config_path()`; `load_or_bootstrap()`; `into_wire()` normalisation; unit tests. |
| `crates/roy/templates/agents_sample.toml` | Fully-commented bootstrap content embedded via `include_str!`. |
| `roy-web/src/lib/agents-config.svelte.ts` | Svelte 5 `$state` store for the AgentsList response; `refresh()` over `royClient.call`. |
| `docs/agents-config.md` | User-facing reference: file path, schema, validation rules, error recovery. |

### Modified

| Path | Reason |
|---|---|
| `crates/roy/Cargo.toml` | Add `toml = "0.8"` dependency. |
| `crates/roy/src/lib.rs` | Re-export `AgentPreset`, `AgentInfo`, `ModelInfo`, `AgentsConfigStatus`. |
| `crates/roy/src/control.rs` | Add `ClientCommand::ListAgents`, `ServerEvent::AgentsList`, sub-types, `ErrorCode::ConfigError`. |
| `crates/roy/src/daemon.rs` | Switch `DefaultTransportFactory::build` to consume `AgentPreset` enum; add `handle_list_agents`; integration tests. |
| `crates/roy-cli/src/main.rs` | New `Cmd::Agents { cmd: AgentsCmd }` subcommand mirroring `ProjectsCmd`; `cmd_agents_list`. |
| `crates/roy-cli/src/mcp.rs` | New `roy_list_agents` tool. |
| `roy-web/src/lib/wire.ts` | Mirror the new TS types and union variants. |
| `roy-web/src/lib/ModelPicker.svelte` | `catalog` prop now `AgentInfo[]`; iterate accordingly. |
| `roy-web/src/lib/NewChat.svelte` | Default `agent`/`model` from store; empty/invalid state UI. |
| `roy-web/src/lib/ChatView.svelte` | Source from store; show "(not in config)" tag for stale model. |
| `roy-web/src/lib/state.svelte.ts` | Trigger `agentsConfig.refresh()` after WS connect. |
| `docs/architecture.md` | Add "Agents discovery layer" section. |
| `docs/wire-protocol.md` | Document `ListAgents` / `AgentsList`. |
| `CLAUDE.md` | Footnote under preset table pointing to `docs/agents-config.md`. |

### Deleted

| Path | Reason |
|---|---|
| `roy-web/src/lib/model-catalog.ts` | Source of truth moved to daemon. |

---

## Phase 1 — Parser, validator, sample (Tasks 1–5)

### Task 1: Add `toml` dependency

**Files:**
- Modify: `crates/roy/Cargo.toml`

- [ ] **Step 1: Locate `[dependencies]` block**

Run: `grep -n "^\[dep" crates/roy/Cargo.toml`

- [ ] **Step 2: Add `toml` after `serde_json`**

Edit `crates/roy/Cargo.toml`. After the `serde_json = "1"` line add:

```toml
toml = "0.8"
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p roy`
Expected: success, `Cargo.lock` updated with `toml`.

- [ ] **Step 4: Commit**

```bash
git add crates/roy/Cargo.toml Cargo.lock
git commit -m "feat(deps): add toml crate for agents config parsing"
```

---

### Task 2: Scaffold `agents_config.rs` with types and basic parser

**Files:**
- Create: `crates/roy/src/agents_config.rs`
- Modify: `crates/roy/src/lib.rs`

- [ ] **Step 1: Create `crates/roy/src/agents_config.rs` with types and `parse()`**

```rust
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
```

- [ ] **Step 2: Add `thiserror` to `crates/roy/Cargo.toml` if not present**

Run: `grep '^thiserror' crates/roy/Cargo.toml || echo MISSING`
If MISSING: add `thiserror = "1"` after the `toml` line.

- [ ] **Step 3: Re-export from `crates/roy/src/lib.rs`**

Add after existing `pub use`:

```rust
pub mod agents_config;
pub use agents_config::{AgentEntry, AgentPreset, AgentsConfig, AgentsConfigError, ModelEntry};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy agents_config::tests`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/Cargo.toml crates/roy/src/agents_config.rs crates/roy/src/lib.rs Cargo.lock
git commit -m "feat(roy): scaffold agents_config module with TOML parser"
```

---

### Task 3: Validation rules + normalization

**Files:**
- Modify: `crates/roy/src/agents_config.rs`

- [ ] **Step 1: Add failing tests for validation**

Append to the `tests` module:

```rust
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
    let AgentsConfigError::Validate(msg) = err else { panic!("wrong variant") };
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
    let AgentsConfigError::Validate(msg) = err else { panic!("wrong variant") };
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
    assert!(matches!(cfg.validate(), Err(AgentsConfigError::Validate(_))));
}
```

- [ ] **Step 2: Run them to confirm they fail (no `validate` method yet)**

Run: `cargo test -p roy agents_config::tests::rejects 2>&1 | tail -10`
Expected: compilation errors about missing `validate`.

- [ ] **Step 3: Implement `validate`**

Add to the `impl AgentsConfig` block (above `parse`):

```rust
pub fn validate(&self) -> Result<(), AgentsConfigError> {
    use std::collections::HashSet;

    let mut seen_preset = HashSet::new();
    for agent in &self.agents {
        if !seen_preset.insert(agent.preset) {
            return Err(AgentsConfigError::Validate(format!(
                "duplicate preset '{}'", agent.preset
            )));
        }

        let defaults: Vec<&str> = agent.models.iter()
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
                    "agent '{}': empty model id", agent.preset
                )));
            }
            if !seen_id.insert(m.id.as_str()) {
                return Err(AgentsConfigError::Validate(format!(
                    "agent '{}': duplicate model id '{}'", agent.preset, m.id
                )));
            }
            if let Some(label) = &m.label {
                if label.trim().is_empty() {
                    return Err(AgentsConfigError::Validate(format!(
                        "agent '{}' model '{}': empty label", agent.preset, m.id
                    )));
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run all parser tests**

Run: `cargo test -p roy agents_config`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/agents_config.rs
git commit -m "feat(agents_config): validate unique presets, defaults, ids"
```

---

### Task 4: Sample template file

**Files:**
- Create: `crates/roy/templates/agents_sample.toml`
- Modify: `crates/roy/src/agents_config.rs`

- [ ] **Step 1: Create `crates/roy/templates/agents_sample.toml`**

```toml
# ~/.config/roy/agents.toml
#
# Configured agents for roy. Roy spawns these ACP-adapter binaries — they
# must be installed and authenticated on this machine. Uncomment the
# block(s) for the agents you actually have.
#
# Models are display-only labels — roy doesn't route by model. Pick the
# string that matches what the underlying agent expects internally (e.g.
# `claude-sonnet-4-6` for claude-code-acp's slash-command).

# ---------- claude-code-acp ----------
# [[agent]]
# preset = "claude"
#
# [[agent.models]]
# id = "claude-sonnet-4-6"
# label = "Claude Sonnet 4.6"
# default = true
#
# [[agent.models]]
# id = "claude-opus-4-7"
# label = "Claude Opus 4.7"
#
# [[agent.models]]
# id = "claude-opus-4-6"
# label = "Claude Opus 4.6"
#
# [[agent.models]]
# id = "claude-opus-4-5"
# label = "Claude Opus 4.5"
#
# [[agent.models]]
# id = "claude-haiku-4-5"
# label = "Claude Haiku 4.5"

# ---------- gemini --acp --skip-trust ----------
# [[agent]]
# preset = "gemini"
#
# [[agent.models]]
# id = "gemini-2.5-pro"
# label = "Gemini 2.5 Pro"
# default = true
#
# [[agent.models]]
# id = "gemini-2.5-flash"
# label = "Gemini 2.5 Flash"

# ---------- codex-acp ----------
# [[agent]]
# preset = "codex"
#
# [[agent.models]]
# id = "gpt-5.4"
# label = "GPT-5.4"
# default = true
#
# [[agent.models]]
# id = "gpt-5.4-mini"
# label = "GPT-5.4 mini"
#
# [[agent.models]]
# id = "gpt-5.3-codex"
# label = "GPT-5.3 Codex"
#
# [[agent.models]]
# id = "gpt-5.3-codex-spark"
# label = "GPT-5.3 Codex Spark"

# ---------- opencode acp (Kimi/Moonshot) ----------
# [[agent]]
# preset = "opencode"
#
# [[agent.models]]
# id = "kimi-for-coding/kimi-k2-thinking"
# label = "Kimi K2 Thinking"
# default = true
#
# [[agent.models]]
# id = "kimi-for-coding/k2p6"
# label = "Kimi K2 p6"
#
# [[agent.models]]
# id = "kimi-for-coding/k2p5"
# label = "Kimi K2 p5"
#
# [[agent.models]]
# id = "kimi/kimi-k2-turbo-preview"
# label = "Kimi K2 Turbo"
#
# [[agent.models]]
# id = "kimi/kimi-k2-0905-preview"
# label = "Kimi K2 0905"
```

- [ ] **Step 2: Embed the sample into `agents_config.rs`**

At the top of `agents_config.rs` (below `use`):

```rust
pub const SAMPLE_TOML: &str = include_str!("../templates/agents_sample.toml");
```

- [ ] **Step 3: Add test that sample parses cleanly**

```rust
#[test]
fn sample_file_parses_to_empty_config() {
    // The committed sample is fully commented, so it should parse and
    // validate as an empty config. This guards against typos in the
    // sample file landing in a release.
    let cfg = AgentsConfig::parse(SAMPLE_TOML).expect("sample parses");
    cfg.validate().expect("sample validates");
    assert!(cfg.agents.is_empty(), "sample must be fully commented out");
}
```

- [ ] **Step 4: Run test**

Run: `cargo test -p roy agents_config::tests::sample_file_parses`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/templates/agents_sample.toml crates/roy/src/agents_config.rs
git commit -m "feat(agents_config): embed sample TOML for bootstrap"
```

---

### Task 5: `config_path()` + `load_or_bootstrap()`

**Files:**
- Modify: `crates/roy/src/agents_config.rs`

- [ ] **Step 1: Add the resolver and outcome enum**

Append to `agents_config.rs`:

```rust
use std::path::{Path, PathBuf};

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
    Ok(PathBuf::from(home).join(".config").join("roy").join("agents.toml"))
}

/// Atomic write: temp file + rename. Crash-safe; concurrent callers race
/// on `rename` and the loser silently overwrites with identical content.
async fn write_sample(path: &Path) -> Result<(), AgentsConfigError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, SAMPLE_TOML).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Read+parse+validate the config at `path`. If the file is missing, write
/// the sample and return `Created` (with no parsed config — the sample is
/// entirely commented and would yield an empty config; we surface the
/// "first run" signal instead).
pub async fn load_or_bootstrap(path: &Path) -> Result<LoadOutcome, AgentsConfigError> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => {
            let cfg = AgentsConfig::parse(&text)?;
            cfg.validate()?;
            Ok(LoadOutcome::Ok(cfg))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_sample(path).await?;
            Ok(LoadOutcome::Created)
        }
        Err(e) => Err(AgentsConfigError::Io(e)),
    }
}
```

- [ ] **Step 2: Add tests covering bootstrap + read paths**

```rust
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
    tokio::fs::write(&path, r#"
        [[agent]]
        preset = "gemini"
        [[agent.models]]
        id = "gemini-2.5-pro"
        default = true
    "#).await.unwrap();
    let outcome = load_or_bootstrap(&path).await.unwrap();
    let LoadOutcome::Ok(cfg) = outcome else { panic!("expected Ok") };
    assert_eq!(cfg.agents.len(), 1);
    assert_eq!(cfg.agents[0].preset, AgentPreset::Gemini);
}

#[tokio::test]
async fn surfaces_validation_error_on_load() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("agents.toml");
    tokio::fs::write(&path, r#"
        [[agent]]
        preset = "claude"
        [[agent]]
        preset = "claude"
    "#).await.unwrap();
    let err = load_or_bootstrap(&path).await.unwrap_err();
    assert!(matches!(err, AgentsConfigError::Validate(_)));
}
```

- [ ] **Step 3: Add `tempfile` to `[dev-dependencies]` if missing**

Run: `grep '^tempfile' crates/roy/Cargo.toml || echo NEED`
If NEED: add `tempfile = "3"` under `[dev-dependencies]` (create the section if missing).

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy agents_config`
Expected: all parser + validator + bootstrap tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/Cargo.toml crates/roy/src/agents_config.rs Cargo.lock
git commit -m "feat(agents_config): config_path resolver and bootstrap-on-missing"
```

---

## Phase 2 — Wire protocol & daemon (Tasks 6–9)

### Task 6: Wire types in `control.rs`

**Files:**
- Modify: `crates/roy/src/control.rs`
- Modify: `crates/roy/src/lib.rs`

- [ ] **Step 1: Add wire-side `AgentInfo`, `ModelInfo`, `AgentsConfigStatus`**

In `agents_config.rs`, append:

```rust
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
        self.agents.into_iter().map(|a| {
            let any_default = a.models.iter().any(|m| m.default);
            let models = a.models.into_iter().enumerate().map(|(i, m)| {
                let label = m.label.unwrap_or_else(|| m.id.clone());
                let default = m.default || (!any_default && i == 0);
                ModelInfo { id: m.id, label, default }
            }).collect();
            AgentInfo { preset: a.preset, models }
        }).collect()
    }
}
```

- [ ] **Step 2: Add tests for normalisation**

```rust
#[test]
fn into_wire_fills_label_from_id() {
    let cfg = AgentsConfig::parse(r#"
        [[agent]]
        preset = "claude"
        [[agent.models]]
        id = "x"
    "#).unwrap();
    let wire = cfg.into_wire();
    assert_eq!(wire[0].models[0].label, "x");
}

#[test]
fn into_wire_promotes_first_model_when_no_default() {
    let cfg = AgentsConfig::parse(r#"
        [[agent]]
        preset = "claude"
        [[agent.models]]
        id = "a"
        [[agent.models]]
        id = "b"
    "#).unwrap();
    let wire = cfg.into_wire();
    assert!(wire[0].models[0].default);
    assert!(!wire[0].models[1].default);
}

#[test]
fn into_wire_preserves_explicit_default() {
    let cfg = AgentsConfig::parse(r#"
        [[agent]]
        preset = "claude"
        [[agent.models]]
        id = "a"
        [[agent.models]]
        id = "b"
        default = true
    "#).unwrap();
    let wire = cfg.into_wire();
    assert!(!wire[0].models[0].default);
    assert!(wire[0].models[1].default);
}
```

- [ ] **Step 3: Add `ClientCommand::ListAgents` and `ServerEvent::AgentsList`**

In `crates/roy/src/control.rs`, add to the `ClientCommand` enum (alphabetically or grouped with other `List*`):

```rust
/// Read `~/.config/roy/agents.toml` (creating a sample if missing) and
/// return the configured agents + models. Pull-only: clients call this
/// whenever they want fresh data.
ListAgents,
```

And to `ServerEvent`:

```rust
/// Response to `ListAgents`. `agents` is empty when `status` is `Created`
/// or `Invalid`; `config_path` is always the resolved path even on errors
/// so the UI can show it.
AgentsList {
    agents: Vec<crate::agents_config::AgentInfo>,
    config_path: std::path::PathBuf,
    status: crate::agents_config::AgentsConfigStatus,
},
```

- [ ] **Step 4: Add `ErrorCode::ConfigError`**

In `ErrorCode` enum (around line 31):

```rust
/// I/O failure reading/writing `agents.toml` (permission denied, disk
/// full, etc.). Parse and validation errors do NOT use this code —
/// they're surfaced via `AgentsList { status: Invalid }`.
ConfigError,
```

Add to the `as_str` / `from_str` mappings:

```rust
ErrorCode::ConfigError => "config_error",
// ...
"config_error" => ErrorCode::ConfigError,
```

- [ ] **Step 5: Wire round-trip test**

In the existing `tests` mod at the bottom of `control.rs`:

```rust
#[test]
fn list_agents_roundtrips() {
    let cmd = ClientCommand::ListAgents;
    let s = serde_json::to_string(&cmd).unwrap();
    let back: ClientCommand = serde_json::from_str(&s).unwrap();
    assert!(matches!(back, ClientCommand::ListAgents));
}

#[test]
fn agents_list_event_roundtrips() {
    use crate::agents_config::{AgentInfo, AgentPreset, AgentsConfigStatus, ModelInfo};
    let ev = ServerEvent::AgentsList {
        agents: vec![AgentInfo {
            preset: AgentPreset::Claude,
            models: vec![ModelInfo {
                id: "claude-sonnet-4-6".into(),
                label: "Claude Sonnet 4.6".into(),
                default: true,
            }],
        }],
        config_path: "/tmp/agents.toml".into(),
        status: AgentsConfigStatus::Ok,
    };
    let s = serde_json::to_string(&ev).unwrap();
    let back: ServerEvent = serde_json::from_str(&s).unwrap();
    let ServerEvent::AgentsList { agents, status, .. } = back else { panic!() };
    assert_eq!(agents.len(), 1);
    assert!(matches!(status, AgentsConfigStatus::Ok));
}
```

- [ ] **Step 6: Re-export from `lib.rs`**

In `crates/roy/src/lib.rs`, expand the `agents_config` re-export:

```rust
pub use agents_config::{
    AgentEntry, AgentInfo, AgentPreset, AgentsConfig, AgentsConfigError,
    AgentsConfigStatus, LoadOutcome, ModelEntry, ModelInfo,
};
```

- [ ] **Step 7: Compile and test**

Run: `cargo test -p roy control::tests agents_config`
Expected: pass.

- [ ] **Step 8: Commit**

```bash
git add crates/roy/src/agents_config.rs crates/roy/src/control.rs crates/roy/src/lib.rs
git commit -m "feat(control): ListAgents/AgentsList wire shapes and ConfigError code"
```

---

### Task 7: Migrate `DefaultTransportFactory` to `AgentPreset` enum

**Files:**
- Modify: `crates/roy/src/daemon.rs:122-154`
- Possibly modify call sites if any pass `&str`.

This is the real refactor called out in CLAUDE.md ("real refactors over awkward preservation"). The old string `match` becomes a sound exhaustive match on the enum; opening for a 5th agent later requires extending one enum, not adding ad-hoc strings in two places.

- [ ] **Step 1: Look at the call sites**

Run: `grep -rn "TransportFactory" crates/roy/src crates/roy/tests`
Note which callers pass strings.

- [ ] **Step 2: Update the `TransportFactory` trait signature**

In `crates/roy/src/daemon.rs` (and wherever the trait is declared — check `manager.rs` too):

```rust
pub trait TransportFactory: Send + Sync {
    fn build(
        &self,
        agent: AgentPreset,
        _model: Option<&str>,
        permission: Option<&str>,
    ) -> Result<Arc<dyn Transport>>;
}
```

- [ ] **Step 3: Update `DefaultTransportFactory::build`**

Replace the existing match at `daemon.rs:132-140`:

```rust
let mut config = match agent {
    AgentPreset::Claude => AcpConfig::claude(),
    AgentPreset::Gemini => AcpConfig::gemini(),
    AgentPreset::Opencode => AcpConfig::opencode(),
    AgentPreset::Codex => AcpConfig::codex(),
};
```

The fallback arm with `unknown agent: {other}` disappears — the enum makes it impossible.

- [ ] **Step 4: Update every call site to parse the string into the enum at the boundary**

Likely callers: `SessionManager::spawn`, `Daemon::handle_run`. The wire-side `SessionSpawnConfig.agent: String` stays — it's user input. Add a parse step:

```rust
let preset: AgentPreset = serde_json::from_value(serde_json::Value::String(agent_str.to_lowercase()))
    .map_err(|_| RoyError::Protocol(format!("unknown agent: {agent_str}")))?;
```

Or simpler — implement `FromStr` on `AgentPreset` in `agents_config.rs`:

```rust
impl std::str::FromStr for AgentPreset {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude"   => Ok(AgentPreset::Claude),
            "gemini"   => Ok(AgentPreset::Gemini),
            "opencode" => Ok(AgentPreset::Opencode),
            "codex"    => Ok(AgentPreset::Codex),
            other      => Err(format!("unknown agent: {other}")),
        }
    }
}
```

Then in call sites: `let preset: AgentPreset = agent_str.parse().map_err(RoyError::Protocol)?;`

- [ ] **Step 5: Re-run the whole workspace test suite**

Run: `cargo test --workspace --no-fail-fast 2>&1 | tail -40`
Expected: pre-existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/daemon.rs crates/roy/src/agents_config.rs crates/roy/src/manager.rs
git commit -m "refactor(daemon): switch TransportFactory to AgentPreset enum"
```

---

### Task 8: `handle_list_agents` + dispatch

**Files:**
- Modify: `crates/roy/src/daemon.rs`

- [ ] **Step 1: Add the handler**

In `daemon.rs` near the other `handle_*` methods:

```rust
async fn handle_list_agents(self: &Arc<Self>, event_tx: &EventTx) {
    use crate::agents_config::{config_path, load_or_bootstrap, AgentsConfigError, AgentsConfigStatus, LoadOutcome};

    let path = match config_path() {
        Ok(p) => p,
        Err(e) => {
            send_error(event_tx, None, ErrorCode::ConfigError,
                &format!("resolve config path: {e}"));
            return;
        }
    };

    let (agents, status) = match load_or_bootstrap(&path).await {
        Ok(LoadOutcome::Ok(cfg))   => (cfg.into_wire(), AgentsConfigStatus::Ok),
        Ok(LoadOutcome::Created)   => (vec![], AgentsConfigStatus::Created),
        Err(AgentsConfigError::Parse(e))    => (vec![], AgentsConfigStatus::Invalid {
            reason: format!("toml parse error: {e}"),
        }),
        Err(AgentsConfigError::Validate(s)) => (vec![], AgentsConfigStatus::Invalid {
            reason: s,
        }),
        Err(AgentsConfigError::Io(e)) => {
            send_error(event_tx, None, ErrorCode::ConfigError,
                &format!("config io error at {}: {e}", path.display()));
            return;
        }
    };

    let _ = event_tx.send(ServerEvent::AgentsList {
        agents,
        config_path: path,
        status,
    });
}
```

- [ ] **Step 2: Dispatch in `handle` (`daemon.rs:437`)**

Add a new arm:

```rust
ClientCommand::ListAgents => self.handle_list_agents(event_tx).await,
```

- [ ] **Step 3: Compile**

Run: `cargo build -p roy`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/roy/src/daemon.rs
git commit -m "feat(daemon): handle_list_agents reads agents.toml on demand"
```

---

### Task 9: Daemon integration tests

**Files:**
- Modify: `crates/roy/src/daemon.rs` (existing `#[cfg(test)] mod tests`)

These tests drive the full Unix-socket path via `tokio::io::duplex`, the same pattern already used in `daemon.rs::tests`.

- [ ] **Step 1: Add helper for ROY_AGENTS_CONFIG override**

The handler reads `ROY_AGENTS_CONFIG` via `config_path()`. To keep tests parallel-safe, each test must set this to a unique temp path AND restore after. Use `temp_env::with_var` from the `temp-env` crate, or set+unset manually in a guard struct.

Add to `[dev-dependencies]` in `crates/roy/Cargo.toml`:

```toml
temp-env = "0.3"
```

- [ ] **Step 2: Add the four integration tests**

In the existing `mod tests`:

```rust
use crate::agents_config::AgentsConfigStatus;

#[tokio::test]
async fn list_agents_returns_ok_for_valid_file() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("agents.toml");
    tokio::fs::write(&cfg_path, r#"
        [[agent]]
        preset = "claude"
        [[agent.models]]
        id = "claude-sonnet-4-6"
        default = true
    "#).await.unwrap();

    temp_env::async_with_vars(
        [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
        async {
            let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
            let ServerEvent::AgentsList { agents, status, .. } = ev else {
                panic!("got {ev:?}");
            };
            assert!(matches!(status, AgentsConfigStatus::Ok));
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].preset, crate::agents_config::AgentPreset::Claude);
        },
    ).await;
}

#[tokio::test]
async fn list_agents_bootstraps_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("missing.toml");
    temp_env::async_with_vars(
        [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
        async {
            let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
            let ServerEvent::AgentsList { agents, status, .. } = ev else { panic!() };
            assert!(matches!(status, AgentsConfigStatus::Created));
            assert!(agents.is_empty());
            assert!(cfg_path.exists());
        },
    ).await;
}

#[tokio::test]
async fn list_agents_reports_invalid_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("agents.toml");
    tokio::fs::write(&cfg_path, "this is not toml [[[").await.unwrap();
    temp_env::async_with_vars(
        [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
        async {
            let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
            let ServerEvent::AgentsList { status, agents, .. } = ev else { panic!() };
            assert!(agents.is_empty());
            assert!(matches!(status, AgentsConfigStatus::Invalid { .. }));
        },
    ).await;
}

#[tokio::test]
async fn list_agents_reports_validation_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("agents.toml");
    tokio::fs::write(&cfg_path, r#"
        [[agent]]
        preset = "claude"
        [[agent]]
        preset = "claude"
    "#).await.unwrap();
    temp_env::async_with_vars(
        [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
        async {
            let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
            let ServerEvent::AgentsList { status, .. } = ev else { panic!() };
            let AgentsConfigStatus::Invalid { reason } = status else { panic!() };
            assert!(reason.contains("duplicate"), "got: {reason}");
        },
    ).await;
}

#[tokio::test]
async fn list_agents_concurrent_bootstrap_is_safe() {
    // Two tasks race on a clean config path. Atomic rename means the
    // "loser" silently overwrites with identical sample content. Both
    // must return Created, neither may panic, the file must end up
    // readable and equal to SAMPLE_TOML.
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("missing.toml");
    temp_env::async_with_vars(
        [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
        async {
            let (a, b) = tokio::join!(
                run_command_against_daemon(ClientCommand::ListAgents),
                run_command_against_daemon(ClientCommand::ListAgents),
            );
            for ev in [a, b] {
                let ServerEvent::AgentsList { status, .. } = ev else { panic!() };
                assert!(matches!(status, AgentsConfigStatus::Created));
            }
            let written = tokio::fs::read_to_string(&cfg_path).await.unwrap();
            assert_eq!(written, crate::agents_config::SAMPLE_TOML);
        },
    ).await;
}
```

If `run_command_against_daemon(cmd)` does not already exist in the test mod, write a 20-line helper that spins up a `Daemon` on a `tokio::io::duplex` pair, sends one command, reads one event, returns it. Pattern after the existing dispatch tests (search `tokio::io::duplex` in the file).

- [ ] **Step 3: Run them**

Run: `cargo test -p roy daemon::tests::list_agents`
Expected: 4 tests pass.

- [ ] **Step 4: Run full workspace test once to catch regressions**

Run: `cargo test --workspace --no-fail-fast`
Expected: no new failures.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/Cargo.toml crates/roy/src/daemon.rs Cargo.lock
git commit -m "test(daemon): integration coverage for ListAgents handler"
```

---

## Phase 3 — CLI + MCP (Tasks 10–11)

### Task 10: `roy agents list` subcommand

**Files:**
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Add the subcommand enum + args**

Near `ProjectsCmd` (main.rs:172):

```rust
#[derive(Subcommand)]
enum AgentsCmd {
    /// List configured agents (and optionally their models).
    List(AgentsListArgs),
}

#[derive(clap::Args)]
struct AgentsListArgs {
    /// One row per (agent, model) instead of summary per agent.
    #[arg(long)]
    models: bool,
    /// Machine-readable JSON output — the full AgentsList event.
    #[arg(long)]
    json: bool,
}
```

- [ ] **Step 2: Add the `Cmd::Agents` variant**

In the `enum Cmd { ... }`:

```rust
/// Inspect configured agents at `~/.config/roy/agents.toml`.
Agents {
    #[command(subcommand)]
    cmd: AgentsCmd,
},
```

- [ ] **Step 3: Dispatch**

In `dispatch` (main.rs:211), add:

```rust
Cmd::Agents { cmd } => cmd_agents(cmd).await,
```

- [ ] **Step 4: Implement `cmd_agents` and `cmd_agents_list`**

```rust
async fn cmd_agents(cmd: AgentsCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        AgentsCmd::List(args) => cmd_agents_list(args).await,
    }
}

async fn cmd_agents_list(args: AgentsListArgs) -> anyhow::Result<ExitCode> {
    use roy::AgentsConfigStatus;

    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    send_cmd(&mut writer, &ClientCommand::ListAgents).await?;
    let ev = read_event(&mut events).await?;
    let ServerEvent::AgentsList { agents, config_path, status } = ev else {
        anyhow::bail!("unexpected response to ListAgents: {ev:?}");
    };

    if args.json {
        let payload = serde_json::json!({
            "agents": agents,
            "config_path": config_path,
            "status": status,
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(ExitCode::SUCCESS);
    }

    match &status {
        AgentsConfigStatus::Created => {
            eprintln!("created sample at {}", config_path.display());
        }
        AgentsConfigStatus::Invalid { reason } => {
            eprintln!("config invalid ({}): {reason}", config_path.display());
            return Ok(ExitCode::from(1));
        }
        AgentsConfigStatus::Ok if agents.is_empty() => {
            eprintln!("no agents configured in {}", config_path.display());
        }
        AgentsConfigStatus::Ok => {}
    }

    if args.models {
        for a in &agents {
            for m in &a.models {
                let mark = if m.default { "*default" } else { "" };
                println!("{}\t{}\t{}\t{}", a.preset, m.id, m.label, mark);
            }
        }
    } else {
        for a in &agents {
            let default = a.models.iter()
                .find(|m| m.default)
                .map(|m| m.id.as_str())
                .unwrap_or("-");
            println!("{}\t{} models\t(default: {})", a.preset, a.models.len(), default);
        }
    }
    Ok(ExitCode::SUCCESS)
}
```

- [ ] **Step 5: Smoke-build**

Run: `cargo build -p roy-cli`
Expected: success.

- [ ] **Step 6: Manual smoke against a running daemon**

In one terminal: `cargo run -p roy-cli -- serve`
In another:
```bash
ROY_AGENTS_CONFIG=/tmp/agents.toml cargo run -p roy-cli -- agents list
cargo run -p roy-cli -- agents list --models
cargo run -p roy-cli -- agents list --json
```

Note: daemon reads `ROY_AGENTS_CONFIG` from *its own* env, not the client's. To test the env override end-to-end, set it on the daemon. For the smoke run, use the default `~/.config/roy/agents.toml`.

Expected: first call prints "created sample at …" to stderr; subsequent calls list whatever you've uncommented; `--json` dumps the wire object.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(cli): roy agents list with --models/--json"
```

---

### Task 11: `roy_list_agents` MCP tool

**Files:**
- Modify: `crates/roy-cli/src/mcp.rs`

- [ ] **Step 1: Find an existing list-style tool to mirror**

Run: `grep -n "roy_list_sessions" crates/roy-cli/src/mcp.rs`
Read 30 lines around each hit to understand the pattern (tool metadata array + handler).

- [ ] **Step 2: Add tool metadata**

In whatever array holds the MCP tool descriptors, insert:

```rust
Tool {
    name: "roy_list_agents".to_string(),
    description: "List agents configured in ~/.config/roy/agents.toml \
                  with their available models. Use to discover what \
                  `agent` string and `model` id values are valid for roy_run."
                  .to_string(),
    input_schema: serde_json::json!({"type": "object", "properties": {}}),
},
```

- [ ] **Step 3: Add the handler**

Mirror the existing list-style handlers. Skeleton:

```rust
async fn handle_list_agents(socket: &Path) -> anyhow::Result<serde_json::Value> {
    let stream = connect_to(socket).await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();
    send_cmd(&mut writer, &ClientCommand::ListAgents).await?;
    let ev = read_event(&mut events).await?;
    let ServerEvent::AgentsList { agents, config_path, status } = ev else {
        anyhow::bail!("unexpected event: {ev:?}");
    };
    Ok(serde_json::json!({
        "agents": agents,
        "config_path": config_path,
        "status": status,
    }))
}
```

- [ ] **Step 4: Dispatch in the tool router**

Add a `"roy_list_agents" => handle_list_agents(socket).await,` arm wherever the other tool names branch.

- [ ] **Step 5: Build**

Run: `cargo build -p roy-cli`
Expected: success.

- [ ] **Step 6: Smoke via stdio**

Run: `echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"roy_list_agents","arguments":{}}}' | cargo run -p roy-cli -- mcp 2>/dev/null`
Expected: a JSON-RPC response with `result.content[0].text` containing the AgentsList payload.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-cli/src/mcp.rs
git commit -m "feat(mcp): roy_list_agents discovery tool"
```

---

## Phase 4 — roy-web (Tasks 12–17)

### Task 12: Mirror wire types in `wire.ts`

**Files:**
- Modify: `roy-web/src/lib/wire.ts`

- [ ] **Step 1: Add the new types**

After the existing `AgentPreset` declaration:

```ts
export interface ModelInfo {
  id: string;
  label: string;
  default: boolean;
}

export interface AgentInfo {
  preset: AgentPreset;
  models: ModelInfo[];
}

export type AgentsConfigStatus =
  | { kind: 'ok' }
  | { kind: 'created' }
  | { kind: 'invalid'; reason: string };
```

- [ ] **Step 2: Extend the `ClientCommand` union**

Add the variant in the union:

```ts
| { op: 'list_agents' }
```

- [ ] **Step 3: Extend the `ServerEvent` union**

```ts
| {
    kind: 'agents_list';
    agents: AgentInfo[];
    config_path: string;
    status: AgentsConfigStatus;
  }
```

- [ ] **Step 4: Update `ServerEventKind`**

If `ServerEventKind` is a `keyof`-style derived type, no change needed. If it's hand-written (likely a string-literal union), append `'agents_list'`.

Run: `grep -n "ServerEventKind" roy-web/src/lib/wire.ts`

- [ ] **Step 5: Build the web project**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run build 2>&1 | tail -20`
Expected: passes (only `wire.ts` changed, no consumers yet).

- [ ] **Step 6: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/wire.ts
git commit -m "feat(web): mirror AgentsList wire types"
```

---

### Task 13: Create `agents-config.svelte.ts` store

**Files:**
- Create: `roy-web/src/lib/agents-config.svelte.ts`

- [ ] **Step 1: Write the store**

```ts
// src/lib/agents-config.svelte.ts
//
// Reactive store mirroring the daemon's AgentsList response. Replaces the
// previous hardcoded `model-catalog.ts`. Call `refresh()` on first WS
// connect and whenever the user wants to re-read the file.

import type { AgentInfo, AgentsConfigStatus } from './wire';
import { royClient } from './client';

class AgentsConfigState {
  agents = $state<AgentInfo[]>([]);
  configPath = $state('');
  status = $state<AgentsConfigStatus>({ kind: 'ok' });
  loading = $state(false);
  lastError = $state<string | null>(null);

  async refresh(): Promise<void> {
    this.loading = true;
    this.lastError = null;
    try {
      const ev = await royClient.call({ op: 'list_agents' }, 'agents_list');
      this.agents = ev.agents;
      this.configPath = ev.config_path;
      this.status = ev.status;
    } catch (e) {
      this.lastError = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }
}

export const agentsConfig = new AgentsConfigState();
```

- [ ] **Step 2: Trigger refresh after WS connect**

Find the existing post-connect hook in `state.svelte.ts` (`grep -n "subscribeFrames\|connect\|ListSessions" roy-web/src/lib/state.svelte.ts`). Add `agentsConfig.refresh()` next to the existing post-connect calls (e.g. session list load).

- [ ] **Step 3: Build**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run build 2>&1 | tail -20`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/agents-config.svelte.ts src/lib/state.svelte.ts
git commit -m "feat(web): agents-config $state store with refresh()"
```

---

### Task 14: Adapt `ModelPicker.svelte` to new shape

**Files:**
- Modify: `roy-web/src/lib/ModelPicker.svelte`

- [ ] **Step 1: Change the `catalog` prop type**

Update the typed `$props()` at line ~30:

```ts
let {
  agent = $bindable(),
  model = $bindable(),
  catalog,
  disabled = false,
  lockAgent = false,
  onChange,
}: {
  agent: AgentPreset;
  model: string;
  catalog: AgentInfo[];   // was: Record<AgentPreset, ModelInfo[]>
  disabled?: boolean;
  lockAgent?: boolean;
  onChange?: (model: string) => void;
} = $props();
```

Import: `import type { AgentInfo, ModelInfo } from './wire';`

- [ ] **Step 2: Replace lookups by preset key with `find` on the array**

Old: `catalog[railAgent]` → New:

```ts
const currentList = $derived(
  catalog.find((a) => a.preset === railAgent)?.models ?? []
);
const currentInfo = $derived(
  (catalog.find((a) => a.preset === agent)?.models ?? [])
    .find((m) => m.id === model)
);
```

(`currentInfo` was previously matching by `m.value`; the new wire shape uses `m.id`.)

- [ ] **Step 3: Replace any `.value` reference with `.id`**

Run: `grep -n "\.value" roy-web/src/lib/ModelPicker.svelte`
Replace each `m.value` → `m.id` and update any usage of the previous local `ModelInfo` type alias.

- [ ] **Step 4: Keep `agentMeta` unchanged**

`agentMeta` (ModelPicker.svelte:57-62) stays — it's a brand table per preset, not derived from config.

- [ ] **Step 5: Build**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run build 2>&1 | tail -20`
Expected: passes (NewChat/ChatView will be updated in next tasks).

- [ ] **Step 6: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/ModelPicker.svelte
git commit -m "feat(web): ModelPicker consumes AgentInfo[] catalog shape"
```

---

### Task 15: Adapt `NewChat.svelte` + add empty/error states

**Files:**
- Modify: `roy-web/src/lib/NewChat.svelte`

- [ ] **Step 1: Replace `MODELS_BY_AGENT` import with store**

Old:
```ts
import { MODELS_BY_AGENT } from './model-catalog';
```
New:
```ts
import { agentsConfig } from './agents-config.svelte';
```

- [ ] **Step 2: Recompute defaults from the store**

Replace lines 19-20 and 31 (`let agent = $state<AgentPreset>('opencode'); let model = $state(...)`) with:

```ts
let agent = $state<AgentPreset>('opencode');
let model = $state<string>('');

// Initialise from the store the first time it has data, and re-sync if
// the user picks an agent that doesn't carry the previous model.
$effect(() => {
  if (!agentsConfig.agents.length) return;
  const found = agentsConfig.agents.find((a) => a.preset === agent)
    ?? agentsConfig.agents[0];
  agent = found.preset;
  if (!found.models.some((m) => m.id === model)) {
    model = (found.models.find((m) => m.default) ?? found.models[0])?.id ?? '';
  }
});
```

- [ ] **Step 3: Open the dialog → refresh the store**

If `NewChat` has an `open` $state or an `onOpen` callback, call `agentsConfig.refresh()` there. Otherwise call it on mount.

- [ ] **Step 4: Render empty/error states above the picker**

At the top of the form body:

```svelte
{#if agentsConfig.status.kind === 'invalid'}
  <div class="error-banner">
    Config error: {agentsConfig.status.reason}
    <code>{agentsConfig.configPath}</code>
    <button onclick={() => agentsConfig.refresh()}>Refresh</button>
  </div>
{:else if agentsConfig.agents.length === 0}
  <div class="empty-state">
    {#if agentsConfig.status.kind === 'created'}
      Created a sample config at <code>{agentsConfig.configPath}</code>.
    {:else}
      No agents configured in <code>{agentsConfig.configPath}</code>.
    {/if}
    Uncomment an agent block and refresh.
    <button onclick={() => agentsConfig.refresh()}>Refresh</button>
  </div>
{:else}
  <ModelPicker bind:agent bind:model catalog={agentsConfig.agents} disabled={submitting} />
{/if}
```

Style classes (`error-banner`, `empty-state`) use the existing Tailwind tokens — copy patterns from a nearby error display in the codebase.

- [ ] **Step 5: Build**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run build 2>&1 | tail -20`
Expected: passes.

- [ ] **Step 6: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/NewChat.svelte
git commit -m "feat(web): NewChat reads agents from store + empty/error UI"
```

---

### Task 16: Adapt `ChatView.svelte` + stale-model badge

**Files:**
- Modify: `roy-web/src/lib/ChatView.svelte`

- [ ] **Step 1: Source `catalog` from the store**

Replace the `MODELS_BY_AGENT` reference with `agentsConfig.agents`. The `ModelPicker` call already uses `lockAgent={true}` and `onChange` to push `set_model`.

- [ ] **Step 2: Inject the session's current model if it's missing**

If the session has `model = "claude-opus-4-6"` and that id is no longer in the configured list, the picker would otherwise show nothing selected. Build an "effective" agent entry that prepends the current model:

```ts
const effectiveCatalog = $derived.by(() => {
  const sessionModel = currentSession.model;
  if (!sessionModel) return agentsConfig.agents;
  return agentsConfig.agents.map((a) => {
    if (a.preset !== currentSession.agent) return a;
    const hasIt = a.models.some((m) => m.id === sessionModel);
    if (hasIt) return a;
    return {
      ...a,
      models: [{ id: sessionModel, label: `${sessionModel} (not in config)`, default: false }, ...a.models],
    };
  });
});
```

Pass `catalog={effectiveCatalog}` instead of `agentsConfig.agents`.

- [ ] **Step 3: Build**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run build 2>&1 | tail -20`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/ChatView.svelte
git commit -m "feat(web): ChatView sources catalog from store, marks stale model"
```

---

### Task 17: Delete `model-catalog.ts`

**Files:**
- Delete: `roy-web/src/lib/model-catalog.ts`

- [ ] **Step 1: Confirm no remaining imports**

Run: `grep -rn "model-catalog\|MODELS_BY_AGENT" roy-web/src`
Expected: no hits.

- [ ] **Step 2: Delete the file**

Run: `rm roy-web/src/lib/model-catalog.ts`

- [ ] **Step 3: Rebuild**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run build 2>&1 | tail -20`
Expected: passes.

- [ ] **Step 4: Browser smoke-test**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run dev`
Open the local URL.
Verify in the browser:
1. With no `~/.config/roy/agents.toml`: NewChat shows "Created a sample at …" banner.
2. After uncommenting `[[agent]] preset = "claude"` block and clicking Refresh: claude appears in the picker.
3. Adding `default = true` to a different model: that model becomes default on refresh.
4. Two `default = true` in the same agent: invalid banner shows the explanatory message.

- [ ] **Step 5: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git rm src/lib/model-catalog.ts
git commit -m "chore(web): remove hardcoded model-catalog"
```

---

## Phase 5 — Docs (Tasks 18–19)

### Task 18: User-facing `docs/agents-config.md`

**Files:**
- Create: `docs/agents-config.md`

- [ ] **Step 1: Write the page**

```markdown
# Agents configuration

Roy reads `~/.config/roy/agents.toml` to decide which ACP agent presets
to surface and what models to list per agent. The file is yours — roy
never overwrites it after the initial bootstrap.

## Resolution order

1. `$ROY_AGENTS_CONFIG` (override; mostly tests and systemd units).
2. `$XDG_CONFIG_HOME/roy/agents.toml`.
3. `~/.config/roy/agents.toml`.

## First run

If the file doesn't exist, the daemon writes a fully-commented sample at
the resolved path and returns `status: created`. The CLI prints
`created sample at <path>` to stderr; the web UI shows a one-line hint.

Open the file, uncomment the blocks for agents you actually have
installed, then refresh in the UI (or just re-run `roy agents list`).

## Format

Each agent is one `[[agent]]` table with a `preset` field and an array
of `[[agent.models]]` sub-tables:

\`\`\`toml
[[agent]]
preset = "claude"   # one of: claude | gemini | opencode | codex

[[agent.models]]
id = "claude-sonnet-4-6"
label = "Claude Sonnet 4.6"
default = true

[[agent.models]]
id = "claude-opus-4-7"
label = "Claude Opus 4.7"
\`\`\`

- `preset` — required, must match one of the four built-in adapter
  presets. Roy spawns the underlying binary (`claude-code-acp`,
  `gemini`, etc.); you're responsible for having it installed and
  authenticated.
- `id` — opaque string passed through as `SessionMetadata.model`. Roy
  itself does *not* route by model; the underlying agent decides what
  the string means.
- `label` — optional human-readable name shown in pickers. Defaults to
  the `id` if omitted.
- `default` — optional; at most one model per agent. If none is marked,
  daemon promotes the first model in the array.

## Validation rules

- Each preset may appear at most once.
- Each model `id` must be unique within its agent.
- At most one model per agent may have `default = true`.
- `id` and `label` (if set) must be non-empty.

Violations surface as `status: invalid { reason }` in the wire response.
The CLI exits with code `1`; the web UI shows a red banner with the
reason and the config path.

## Editing workflow

1. Open `~/.config/roy/agents.toml` in your editor.
2. Save.
3. Refresh: `roy agents list`, or click the refresh button in the web
   picker. Daemon re-reads the file on every call — no daemon restart
   needed.

## Inspecting

\`\`\`bash
roy agents list           # one row per agent: name, model count, default
roy agents list --models  # one row per (agent, model)
roy agents list --json    # full wire payload, for scripts
\`\`\`

MCP clients can call the `roy_list_agents` tool to discover the same
data programmatically.

## What this file does NOT control

- Binary paths, command-line args, ACP mode, permission policy, or any
  other agent-launch knob — these live in Rust (`AcpConfig::*`) and are
  not overridable from this file.
- Whether roy successfully spawns the agent — that depends on the
  binary being on `PATH` at run time. If it's missing, `roy run` fails
  with the OS-level `No such file or directory` error.
- Routing the model into the spawned agent — `model` is a display
  label. The underlying agent picks its model via its own configuration
  (e.g. `/model` slash command in Claude Code).
```

- [ ] **Step 2: Commit**

```bash
git add docs/agents-config.md
git commit -m "docs: agents.toml user reference"
```

---

### Task 19: Architecture + wire-protocol + CLAUDE.md updates

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/wire-protocol.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add "Agents discovery layer" section to `docs/architecture.md`**

Find a natural insertion point (e.g. after the transport layer description). Add a 6–10-line section pointing at `agents_config.rs` and noting that it's stateless and re-read per request.

- [ ] **Step 2: Document `ListAgents`/`AgentsList` in `docs/wire-protocol.md`**

Follow the existing per-command/per-event format in the file. Show the request JSON, the response JSON for each of the three status variants, and the `ErrorCode::ConfigError` case.

- [ ] **Step 3: Footnote under the preset table in `CLAUDE.md`**

After the four-row preset table, add:

```markdown
Which presets and models are *surfaced* to clients is controlled by
`~/.config/roy/agents.toml` (see `docs/agents-config.md`). The four
preset binaries above must still be installed and authenticated.
```

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md docs/wire-protocol.md CLAUDE.md
git commit -m "docs: agents discovery layer in architecture/wire/CLAUDE"
```

---

## Self-review checklist (run before handoff)

- [ ] **Spec coverage** — every section/decision in
  `docs/superpowers/specs/2026-05-24-agents-config-design.md` has a
  task that implements it. Confirm by grep'ing the decision table (#1
  through #8) against the plan.
- [ ] **Placeholder scan** — no "TBD", "TODO", "as appropriate",
  "similar to above", "add error handling".
- [ ] **Type consistency** — `AgentInfo`/`ModelInfo`/`AgentsConfigStatus`
  names match between `agents_config.rs`, `control.rs`, `wire.ts`, and
  every consumer.
- [ ] **`cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`** all pass.
- [ ] **`cd roy-web && npm run build`** passes; smoke in browser per Task 17 Step 4.

---

## Out-of-scope (re-asserted)

Do not implement during this plan, even if tempting:

1. `roy agents add` / `remove` mutation commands.
2. PATH probing or installed-binary checks.
3. File watcher / push notifications when `agents.toml` changes.
4. Custom (5th) agent preset support.
5. Per-agent override of binary path, args, permission policy, env.
6. Web UI for editing `agents.toml`.
7. Schema versioning (`schema_version = 1`).
