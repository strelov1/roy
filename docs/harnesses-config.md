# Harnesses configuration

Roy reads `~/.config/roy/harnesses.toml` to decide which ACP harnesses
to surface and what models to list per harness. The file is yours — roy
never overwrites it after the initial bootstrap.

> A **harness** is one of the ACP-adapter binaries roy spawns
> (`claude-code-acp`, `gemini`, `opencode`, `codex-acp`, `pi-acp`). Don't
> confuse it with an **agent**, which is a persona defined in
> `.roy/agents/<slug>.md` and references a harness + model + prompt.

## Resolution order

1. `$ROY_HARNESSES_CONFIG` (override; mostly tests and systemd units).
2. `$XDG_CONFIG_HOME/roy/harnesses.toml`.
3. `~/.config/roy/harnesses.toml`.

## First run

If the file doesn't exist, the daemon writes a fully-commented sample at
the resolved path and returns `status: created`. The CLI prints
`created sample at <path>` to stderr; the web UI shows a one-line hint.

Open the file, uncomment the blocks for harnesses you actually have
installed, then refresh in the UI (or just re-run `roy harnesses list`).

## Format

Each harness is one `[[harness]]` table with a `name` field and an array
of `[[harness.models]]` sub-tables:

```toml
[[harness]]
name = "claude"   # one of: claude | gemini | opencode | codex | pi

[[harness.models]]
id = "claude-sonnet-4-6"
label = "Claude Sonnet 4.6"
default = true

[[harness.models]]
id = "claude-opus-4-7"
label = "Claude Opus 4.7"
```

- `name` — required, must match one of the five built-in adapter
  harnesses. Roy spawns the underlying binary (`claude-code-acp`,
  `gemini`, etc.); you're responsible for having it installed and
  authenticated.
- `id` — opaque string passed through as `SessionMetadata.model`. Roy
  itself does *not* route by model; the underlying harness decides what
  the string means.
- `label` — optional human-readable name shown in pickers. Defaults to
  the `id` if omitted.
- `default` — optional; at most one model per harness. If none is
  marked, the daemon promotes the first model in the array.

## Validation rules

- Each harness name may appear at most once.
- Each model `id` must be unique within its harness.
- At most one model per harness may have `default = true`.
- `id` and `label` (if set) must be non-empty.

Violations surface as `status: invalid { reason }` in the wire response.
The CLI exits with code `1`; the web UI shows a red banner with the
reason and the config path.

## Editing workflow

1. Open `~/.config/roy/harnesses.toml` in your editor.
2. Save.
3. Refresh: `roy harnesses list`, or click the refresh button in the
   web picker. The daemon re-reads the file on every call — no daemon
   restart needed.

## Inspecting

```bash
roy harnesses list           # one row per harness: name, model count, default
roy harnesses list --models  # one row per (harness, model)
roy harnesses list --json    # full wire payload, for scripts
```

MCP clients can call the `roy_list_harnesses` tool to discover the same
data programmatically.

## What this file does NOT control

- Binary paths, command-line args, ACP mode, permission policy, or any
  other harness-launch knob — these live in Rust (`AcpConfig::*`) and
  are not overridable from this file.
- Whether roy successfully spawns the harness — that depends on the
  binary being on `PATH` at run time. If it's missing, `roy run` fails
  with the OS-level `No such file or directory` error.
- Routing the model into the spawned harness — `model` is a display
  label. The underlying harness picks its model via its own
  configuration (e.g. `/model` slash command in Claude Code).

## Full example

```toml
# ~/.config/roy/harnesses.toml

[[harness]]
name = "claude"

[[harness.models]]
id = "claude-sonnet-4-6"
label = "Claude Sonnet 4.6"
default = true

[[harness.models]]
id = "claude-opus-4-7"
label = "Claude Opus 4.7"

[[harness]]
name = "gemini"

[[harness.models]]
id = "gemini-2.5-pro"
label = "Gemini 2.5 Pro"
default = true
```

After editing the file, refresh the picker in the web UI (the refresh
button next to the empty-state banner, or just re-open `NewChat`) or
re-run `roy harnesses list` from a shell.

## Troubleshooting

**`"No harnesses in ~/.config/roy/harnesses.toml"` even though I see entries.**
Most likely: every `[[harness]]` block is still commented out from the
sample. Remove the leading `# ` from the lines you want active. Run
`roy harnesses list --json` to see what the daemon parses.

**`"config invalid: ..."` banner with a duplicate-default reason.**
Two models in the same harness have `default = true`. Only one is
allowed. Pick one, remove the flag from the other.

**Picker shows a harness but spawning it fails with `No such file or directory`.**
The ACP-adapter binary isn't on `PATH`. Roy does not check for
presence — it only filters what to *show*, not what's *runnable*.
Install the binary (`claude-code-acp`, `gemini`, `opencode`,
`codex-acp`, `pi-acp`) and put it on `PATH`. Verify with
`which claude-code-acp`.

**Switched the picker to a different model, the underlying harness kept the old one.**
`model` is a display label only — roy does not feed it into the
spawned harness process. The harness picks its model through its own
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

