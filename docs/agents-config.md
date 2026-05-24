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
```

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
  the daemon promotes the first model in the array.

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
   picker. The daemon re-reads the file on every call — no daemon
   restart needed.

## Inspecting

```bash
roy agents list           # one row per agent: name, model count, default
roy agents list --models  # one row per (agent, model)
roy agents list --json    # full wire payload, for scripts
```

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
