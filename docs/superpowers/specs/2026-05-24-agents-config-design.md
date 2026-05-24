# Agents configuration — design

Status: draft, awaiting user review.
Date: 2026-05-24.
Author: brainstorm with `strelov1`.

## Goal

Replace the hardcoded agent/model lists currently scattered across
`roy-web/src/lib/model-catalog.ts` (and implicitly across `daemon.rs`,
`mcp.rs`, etc.) with a single user-owned configuration file that
describes which ACP agents the user has installed and what models they
want surfaced for each.

Concrete trigger: the model list changes often, and the current setup
requires a roy-web release every time. The agent list also shows all
four presets even when the user only has Claude and Gemini installed.

## Non-goals

- **No auto-discovery.** Daemon does not probe `PATH` for binaries or
  query agent CLIs for their model list. The user maintains the file.
- **No custom agents.** The four built-in presets (`claude`, `gemini`,
  `opencode`, `codex`) remain hardcoded in Rust. Config is a *filter
  on top of presets*, not a replacement.
- **No override of binary path / args / permission policy / env via
  config.** Preset definitions stay in `AcpConfig::*`. If the user
  needs a different binary path — symlink in `~/.local/bin/`.
- **No CLI mutations** (`roy agents add` / `remove`). Source of truth
  is the file; users edit it in `$EDITOR`.
- **No file watcher / push notifications.** Pull-only. Future upgrade
  possible without contract changes.
- **No web UI for editing `agents.toml`.** Out of scope.
- **No model→transport routing.** As today, `model` stays a display
  label that the daemon stores in `SessionMetadata` but does NOT feed
  into the spawned agent process. (Existing roy behaviour.)
- **No schema versioning** in the first cut. Unknown TOML keys are
  silently ignored by `serde` defaults; we add a version field only
  if and when a breaking change is required.

## Decisions (resolved during brainstorm)

| # | Decision |
|---|---|
| 1 | Source of truth: `~/.config/roy/agents.toml` (XDG, override via `ROY_AGENTS_CONFIG`). |
| 2 | Scope: filter on top of 4 hardcoded presets. User picks which to enable + what models. |
| 3 | Re-read file on every `ListAgents` request. No cache, no watcher. |
| 4 | Per-model schema: rich object `{ id, label?, default? }` in `[[agent.models]]` array-of-tables. |
| 5 | Missing file → daemon writes a fully-commented sample, returns `status: created`. |
| 6 | Exposed via CLI (`roy agents list`), MCP (`roy_list_agents`), and web (`agents-config.svelte.ts`). |
| 7 | One wire response shape for all states: `AgentsList { agents, config_path, status }`. |
| 8 | `AgentPreset` becomes an enum in Rust (single source of truth); existing string-match in `DefaultTransportFactory::build` migrates to it. |

## Architecture

```
~/.config/roy/agents.toml  ← user-edited, daemon-readable
            │
            ▼ read on every ListAgents
┌─────────────────────────────────────────────┐
│ roy daemon                                  │
│  src/agents_config.rs   (parse + validate)  │
│  src/daemon.rs::handle_list_agents()        │
└──┬─────────────┬──────────────────┬─────────┘
   │             │                  │
   ▼             ▼                  ▼
roy mcp     roy CLI            roy-web
roy_list_   roy agents list    agents-config.svelte.ts
agents      [--models|--json]  (replaces model-catalog.ts)
```

## File format

Path resolution (in `agents_config::config_path()`):

1. If `$ROY_AGENTS_CONFIG` is set — use it (testing, systemd).
2. Else `$XDG_CONFIG_HOME/roy/agents.toml`.
3. Else `~/.config/roy/agents.toml` (same on Linux and macOS).

Format — TOML, array-of-tables for stable order:

```toml
[[agent]]
preset = "claude"   # one of: claude | gemini | opencode | codex

[[agent.models]]
id = "claude-sonnet-4-6"
label = "Claude Sonnet 4.6"
default = true

[[agent.models]]
id = "claude-opus-4-7"
label = "Claude Opus 4.7"

[[agent]]
preset = "gemini"

[[agent.models]]
id = "gemini-2.5-pro"
label = "Gemini 2.5 Pro"
default = true
```

## Rust types

New crate-internal module `crates/roy/src/agents_config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentPreset {
    Claude,
    Gemini,
    Opencode,
    Codex,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub default: bool,
}
```

`AgentPreset` is the single source of truth. The existing string-match
in `DefaultTransportFactory::build` (`crates/roy/src/daemon.rs:132-140`)
migrates to take `AgentPreset` directly — real refactor, no shim.

## Validation rules

Applied after parse in `AgentsConfig::validate()`:

1. **Unique preset** across `[[agent]]` sections — duplicates rejected.
2. **At most one `default = true`** per agent — duplicate defaults
   rejected with both IDs in the error message.
3. **Unique model `id`** within an agent.
4. **Non-empty `id` and `label`** if provided.
5. **Normalization** (not failures): if `label` is omitted, daemon
   fills `label = id` in the wire response so clients always get a
   string. If no model is `default = true` in an agent, daemon promotes
   the first model in the array. If `models = []` — agent is valid;
   `default = None` and clients render the agent as "no models yet".

## Errors

```rust
pub enum AgentsConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Validate(String),
}
```

- `Parse` and `Validate` → returned to client as
  `AgentsList { agents: [], status: Invalid { reason } }`. NOT a
  transport error: user mis-config is part of normal operation.
- `Io` (no permission, disk full when writing sample) → returned as
  `ServerEvent::Error { code: ConfigError, message }`. This IS a
  transport-level failure: daemon couldn't reach the file at all.

New `ErrorCode::ConfigError` added to `crates/roy/src/control.rs`.

## Wire protocol

Additions to `crates/roy/src/control.rs`:

```rust
// ClientCommand (tag: "op")
ListAgents,

// ServerEvent (tag: "kind")
AgentsList {
    agents: Vec<AgentInfo>,
    config_path: PathBuf,
    status: AgentsConfigStatus,
},
```

Sub-types (exported from `agents_config.rs`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentsConfigStatus {
    Ok,
    Created,                    // file missing, sample written
    Invalid { reason: String }, // parse or validate failure
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub preset: AgentPreset,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,    // daemon-normalized (= id if user omitted)
    pub default: bool,    // exactly one true per agent (daemon-normalized)
}
```

### Wire example — Ok

```json
{
  "kind": "agents_list",
  "agents": [
    {
      "preset": "claude",
      "models": [
        {"id": "claude-sonnet-4-6", "label": "Claude Sonnet 4.6", "default": true},
        {"id": "claude-opus-4-7",   "label": "Claude Opus 4.7",   "default": false}
      ]
    }
  ],
  "config_path": "/Users/foo/.config/roy/agents.toml",
  "status": {"kind": "ok"}
}
```

### Wire example — Created (bootstrap)

```json
{
  "kind": "agents_list",
  "agents": [],
  "config_path": "/Users/foo/.config/roy/agents.toml",
  "status": {"kind": "created"}
}
```

### Wire example — Invalid

```json
{
  "kind": "agents_list",
  "agents": [],
  "config_path": "/Users/foo/.config/roy/agents.toml",
  "status": {
    "kind": "invalid",
    "reason": "agent 'claude': two models marked default (claude-sonnet-4-6, claude-opus-4-7)"
  }
}
```

### Four UI states

| State | `agents` | `status` | UI |
|-------|----------|----------|----|
| Normal | non-empty | `ok` | Picker as today |
| Empty file | `[]` | `ok` | "No agents in `<config_path>`. Uncomment a preset." |
| Bootstrap | `[]` | `created` | "Sample created at `<config_path>`. Edit and refresh." |
| Invalid | `[]` | `invalid {reason}` | Red banner with reason + path |

## CLI

New nested subcommand group `agents` (mirrors `projects` precedent at
`crates/roy-cli/src/main.rs:169`):

```
roy agents list              # one row per preset: name, model count, default
roy agents list --models     # one row per (preset, model): TSV
roy agents list --json       # full AgentsList JSON on stdout
```

Output rules:

- Stdout: data only (TSV or JSON).
- Stderr: status banners (`created sample at ...`, `no agents configured`,
  `config invalid: ...`).
- Exit `0` for Ok/Created/empty; `1` for Invalid (domain error);
  `2` for CLI-level failure (no daemon, bad flag).

## MCP

New tool `roy_list_agents` in `crates/roy-cli/src/mcp.rs`:

- No input parameters.
- Returns the same JSON shape as `roy agents list --json`.
- Description: "List agents configured in `~/.config/roy/agents.toml`
  with their available models. Use to discover what `agent` string and
  `model` id values are valid for `roy_run`."

## roy-web

### 1. Wire-type mirror in `src/lib/wire.ts`

```ts
export type AgentPreset = 'claude' | 'gemini' | 'opencode' | 'codex';

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

// ClientCommand union — new variant
| { op: 'list_agents' }

// ServerEvent union — new variant
| {
    kind: 'agents_list';
    agents: AgentInfo[];
    config_path: string;
    status: AgentsConfigStatus;
  }
```

### 2. New `$state` store `src/lib/agents-config.svelte.ts`

```ts
class AgentsConfigState {
  agents = $state<AgentInfo[]>([]);
  configPath = $state('');
  status = $state<AgentsConfigStatus>({ kind: 'ok' });
  loading = $state(false);

  async refresh() { /* ListAgents over WS */ }
}

export const agentsConfig = new AgentsConfigState();
```

`refresh()` is called:

- At first WS connect (alongside the existing session-list load).
- When `NewChat` opens (user may have just edited the file).
- From a "refresh" icon button in `ModelPicker`.

### 3. Delete `src/lib/model-catalog.ts`

No more hardcoded `MODELS_BY_AGENT`. All readers migrate to
`agentsConfig.agents`.

### 4. Adapt consumers

- `NewChat.svelte` (current state defaults at lines 19–20, 31):
  default agent = `agentsConfig.agents[0]?.preset`; default model = its
  `models.find(m => m.default)`. Reactive via `$derived`.
- `ModelPicker.svelte`: `catalog` prop type changes from
  `Record<AgentPreset, ModelInfo[]>` to `AgentInfo[]`. Provider/icon
  remains in local `agentMeta` (`ModelPicker.svelte:57-62`) — brand,
  not data.
- `ChatView.svelte`: reads from `agentsConfig.agents`.

### 5. Empty-state UI in `NewChat.svelte`

Renders by `agentsConfig.status.kind` + `agents.length === 0`. Shows
`config_path` and the reason (if Invalid). A "Refresh" button calls
`agentsConfig.refresh()`.

### 6. Stale model on existing session

If a session was opened with `model = "claude-opus-4-6"` and the user
later removes that model from `agents.toml`, the picker in `ChatView`
shows the current model as a leading entry marked "(not in config)".
Two-line UI tweak; daemon contract unchanged.

## Sample file

Stored as `crates/roy/templates/agents_sample.toml`, embedded via
`include_str!`. Fully commented out. Content mirrors the current
`roy-web/src/lib/model-catalog.ts` snapshot:

- 5 Claude models (Sonnet 4.6 default, Opus 4.7/4.6/4.5, Haiku 4.5).
- 2 Gemini models (2.5 Pro default, 2.5 Flash).
- 4 Codex models (gpt-5.4 default, 5.4-mini, 5.3-codex, 5.3-codex-spark).
- 5 OpenCode/Moonshot models (Kimi K2 Thinking default + others).

Sample is **NOT** auto-updated when new roy versions ship — that would
mean silently rewriting the user's file. It's a one-time bootstrap.

## Atomic sample write

```rust
async fn write_sample(path: &Path) -> Result<(), AgentsConfigError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(AgentsConfigError::Io)?;
    }
    let tmp = path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, SAMPLE_TOML).await.map_err(AgentsConfigError::Io)?;
    tokio::fs::rename(&tmp, path).await.map_err(AgentsConfigError::Io)?;
    Ok(())
}
```

Crash-safe: `rename` is atomic on a single filesystem. Concurrent
bootstrap from two clients is harmless — the loser overwrites with
identical content.

## Tests

### Unit (`agents_config.rs`)

- `parses_valid_toml_with_all_fields`
- `parses_models_without_label_fills_id_as_label`
- `parses_models_without_default_picks_first`
- `rejects_duplicate_preset`
- `rejects_two_defaults_in_one_agent`
- `rejects_duplicate_model_id_in_one_agent`
- `rejects_unknown_preset_value`
- `rejects_empty_id_or_label`
- `sample_file_parses_cleanly` (compile-time-bundled sample must always
  parse to an empty config when commented out)

### Daemon integration (`daemon.rs::tests`)

- `list_agents_returns_ok_for_valid_file` (via Unix-socket duplex)
- `list_agents_bootstraps_missing_file`
- `list_agents_reports_invalid_toml`
- `list_agents_reports_validation_error`
- `list_agents_concurrent_bootstrap` (two tokio tasks race on a clean
  config path; both succeed; no panic; file ends up readable)

### CLI

- `roy agents list --json` returns an object with `agents`,
  `config_path`, `status`. Asserted via snapshot or shape-check.

## Docs updates

- `docs/architecture.md`: add "Discovery layer" section referencing
  `agents_config.rs`.
- `docs/wire-protocol.md`: document `ListAgents` / `AgentsList`.
- `CLAUDE.md`: add a sentence under the preset table that pointing at
  `~/.config/roy/agents.toml` and `docs/agents-config.md`.
- New `docs/agents-config.md`: user-facing reference — path, format,
  validation rules, what to do when things break. Linked from CLI
  help, MCP tool description, and web empty-state.

## Out of scope (recap, ranked by likelihood we'll hear about them)

1. `roy agents add` / `remove` mutations — over $EDITOR, not worth it.
2. PATH probing — explicit YAGNI.
3. File watcher / push — pull is enough; contract supports upgrade later.
4. Custom agents (5th preset) — would require preset registry refactor.
5. Per-agent binary path / args overrides — symlink instead.
6. Schema versioning — add `schema_version` only when a breaking change demands it.
