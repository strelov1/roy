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

## Full example

```toml
# ~/.config/roy/agents.toml

[[agent]]
preset = "claude"

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

After editing the file, refresh the picker in the web UI (the refresh
button next to the empty-state banner, or just re-open `NewChat`) or
re-run `roy agents list` from a shell.

## Troubleshooting

**`"No agents in ~/.config/roy/agents.toml"` even though I see agents in the file.**
Most likely: every `[[agent]]` block is still commented out from the
sample. Remove the leading `# ` from the lines you want active. Run
`roy agents list --json` to see what the daemon parses.

**`"config invalid: ..."` banner with a duplicate-default reason.**
Two models in the same agent have `default = true`. Only one is
allowed. Pick one, remove the flag from the other.

**Picker shows an agent but spawning it fails with `No such file or directory`.**
The ACP-adapter binary isn't on `PATH`. Roy does not check for
presence — it only filters what to *show*, not what's *runnable*.
Install the binary (`claude-code-acp`, `gemini`, `opencode`,
`codex-acp`) and put it on `PATH`. Verify with `which claude-code-acp`.

**Switched the picker to a different model, the underlying agent kept the old one.**
`model` is a display label only — roy does not feed it into the
spawned agent process. The agent picks its model through its own
mechanism (e.g. Claude Code's `/model` slash command). Change it
inside the chat to actually swap models.

**Edited the file, picker still shows the old list.**
Hit the refresh button in `NewChat`. The daemon re-reads the file on
every request (no cache), but the web UI only refetches when you tell
it to.

**Two `default = true` got silently merged after I uncommented a block.**
That doesn't happen — it's a hard validation error (`config invalid`
status). If you see the picker working with two defaults, you're
looking at stale data. Refresh.
