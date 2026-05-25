# Design: agents (personas) in roy

Status: draft for review · 2026-05-24

## Problem

Today roy's notion of an "agent" is thin: `~/.config/roy/agents.toml`
(parsed by `agents_config.rs`) is a *capability catalog* — which of the four
ACP presets to surface and which model labels to list per preset. A session
carries `cwd`, `model`, `tags`, `resume_cursor` and nothing resembling a
persona. There is no system/initial prompt anywhere, and no point in the
spawn path to inject one (`engine.rs` simply forwards `Cmd::Prompt`).

We want something closer to Claude Code subagents: a named, reusable **agent**
that owns an *initial/system prompt* plus its CLI (preset) and model — listable
and (later) creatable from a UI. Memory is not a separate object: it falls out
of how the agent is launched (a continued persistent session has memory; a
fresh session does not). The personality is constant either way.

## Non-goals

- A separate "agent management" crate. Personas must be read by the daemon at
  spawn time to inject the prompt, so their definitions live inside `roy`.
  Putting them behind a socket (like `roy-scheduler`/`roy-gateway`) would
  invert the dependency direction the CLAUDE.md boundary rules require.
- A persistent "сотрудник" object with its own table (as in claude-agent's
  `background_agents`). Memory = resume semantics that already exist; the
  scheduler/gateway already own their state and decide whether to reuse a
  session id or spawn fresh.
- Building the web UI itself. This design exposes the daemon API (WS/CLI/MCP)
  that a UI consumes; the UI is out of scope for the `roy` crate.

## Terminology change

The word "agent" is reclaimed for the persona concept (matching CC). The
existing preset+models catalog is renamed to **models**:

| Before | After |
|--------|-------|
| `~/.config/roy/agents.toml` | `~/.config/roy/models.toml` |
| `agents_config.rs` | `models_config.rs` |
| `ClientCommand::ListAgents` | `ClientCommand::ListModels` |
| `ServerEvent::AgentsList { … }` | `ServerEvent::ModelsList { … }` |
| `roy agents [list]` (CLI `AgentsCmd`) | `roy models [list]` (`ModelsCmd`) |
| MCP `roy_list_agents` | MCP `roy_list_models` |
| `AgentInfo` / `AgentsConfig*` (catalog types) | `ModelProviderInfo` / `ModelsConfig*` |

This is a breaking rename of the CLI/config surface, accepted as a real
refactor rather than awkward preservation. `roy agents` / `AgentList` /
`AgentInfo` are then free for the new persona concept.

## Section 1 — Data model & storage

New entity `AgentDef` (a persona) — one file per agent at
`~/.config/roy/agents/<slug>.md`, CC-subagent format:

```markdown
---
name: Reviewer
description: Strict code reviewer
preset: claude            # one of the four presets
model: claude-opus-4-7    # model id (as in models.toml; a display label, not routing)
persistent: false         # optional launch hint read by clients; core ignores it
---
You are a meticulous reviewer. Hunt bugs, not style. ...   ← body = initial/system prompt
```

- New module `crates/roy/src/agent_defs.rs`: parse frontmatter + body,
  validate, load the catalog from a directory. Slug derives from the filename.
- Directory resolution mirrors `models.toml`:
  `$ROY_AGENTS_DIR` → `$XDG_CONFIG_HOME/roy/agents/` → `~/.config/roy/agents/`.
- File-per-agent → safe machine writes from a UI (atomic temp+rename, no
  clobbering neighbours), human-readable, git-friendly.
- Relationship to `models.toml`: it stays the capability catalog. An `AgentDef`
  references `preset` + `model`. Validation is soft — a model absent from the
  catalog is a warning, not an error (model is a display label anyway).

## Section 2 — Persona injection (engine + transport)

`AcpConfig` gains a capability field `system_prompt: SystemPromptChannel`:

- `Meta` (claude, opencode): persona goes into ACP `_meta.systemPrompt =
  { append: <persona> }` on **both `session/new` and `session/load`**. This is
  a real system prompt — outside conversation history, survives resume.
  Confirmed in `claude-code-acp` (`acp-agent.js:756`, honored on new and load;
  Rust SDK exposes `meta: Option<Meta>` on `NewSessionRequest` and
  `LoadSessionRequest`, `agent.rs:954` / `:1136`).
- `FirstTurn` (gemini, codex): on a **fresh** session the engine sends the
  persona as the first prompt, journaled as `TurnEvent::System`. On resume the
  agent reloads history that already contains it — no re-injection.

Rationale for not using gemini/codex native channels now: gemini's
`GEMINI_SYSTEM_MD` is a *full replacement* requiring brittle, version-coupled
reconstruction of the default prompt via `${…}` placeholders; codex has a clean
`developer_instructions` in its SDK but whether `codex-acp` forwards it through
ACP `_meta` is unconfirmed. Both are the kind of clever shim the code-quality
bar forbids. First-turn injection is uniform and reliable; native channels can
be wired later per-adapter if first-turn proves insufficient.

Plumbing:

- `Transport::open` gains `system_prompt: Option<String>`. For `Meta` it sets
  `NewSessionRequest.meta` / `LoadSessionRequest.meta`; for `FirstTurn` it is a
  no-op (the engine owns first-turn injection).
- `SessionMetadata` gains `agent_slug: Option<String>` **and a snapshot of the
  persona body**. The snapshot makes resume stable: re-sending
  `_meta.systemPrompt` on every `session/load` uses the snapshot, so editing or
  deleting the `.md` never mutates or breaks live sessions. Personality is
  fixed at session birth; edits affect only new sessions. The slug is stored
  alongside for display/UI.

## Section 3 — Wire protocol & management (CLI / WS / MCP)

- Catalog rename as per Terminology section (`ListModels` / `ModelsList`).
- New persona commands on `ClientCommand` / `ServerEvent`:
  - `AgentList` → `AgentsDefined { agents: Vec<AgentDefInfo> }`
  - `AgentGet { slug }`
  - `AgentSave { slug, name, description, preset, model, prompt, persistent }`
    → writes `<slug>.md` atomically (temp + rename, like `write_sample`).
  - `AgentDelete { slug }`
- Starting a session from an agent: `Spawn` (and the `Run` composite) gain
  `agent: Option<String>`. When set, the daemon resolves the `AgentDef`, takes
  its preset + model + persona, snapshots the persona into metadata, and applies
  injection. An explicit `model` in the command may override the agent's.
- CLI: `roy run --agent reviewer --cwd …`; `roy agents list|show|new|rm`.
- MCP: add `roy_list_agents` (personas) and an `agent` argument on `roy_run`.
- UI (later): the CRUD WS commands above cover "show and create"; a web client
  consumes them. No core change needed for the UI itself.

## Section 4 — Persistent "сотрудник" & memory

No new object type. Agent = identity; memory = launch mode over existing resume
semantics:

- Fresh: `roy run --agent X` → new session, persona present, no memory.
- With memory: reuse one session and keep resuming it — exactly what the
  scheduler already does via `Fire` into a specific session.

The `.md` may carry an optional `persistent: true` hint that clients
(scheduler/UI) read to decide whether to reuse a session id; the `roy` core
does not enforce or track it. This keeps the core stateless about "сотрудник
memory" and respects the crate boundary rules.

## Section 5 — Testing

- **Unit (`agent_defs.rs`):** frontmatter+body parsing, slug-from-filename,
  missing/empty fields, unknown preset, soft warning for off-catalog model,
  atomic write.
- **Injection (fake agent):** extend `tests/scripts/fake-acp-agent.py` to echo
  the received `_meta`; assert `_meta.systemPrompt` arrives on `session/new`
  AND `session/load` for `Meta`; assert a single leading `System` event for
  `FirstTurn` on fresh spawn and none on resume.
- **Resume durability:** spawn from agent → persona snapshot in metadata → kill
  daemon → resume → `_meta` re-sent from snapshot (Meta) / no duplicate
  first-turn (FirstTurn).
- **Daemon round-trip:** `AgentSave` / `AgentList` / `AgentDelete` over the
  socket; `Run { agent }` resolves the persona.
- **Regression:** existing catalog tests updated to `ModelsList`.
- **Real-CLI smoke (`#[ignore]`):** a persona like "always answer with the word
  FOO" yields a response containing FOO (claude).

## Open questions

- `opencode` `_meta.systemPrompt` support is inferred from binary strings, not
  verified against a running adapter. The `Meta`/`FirstTurn` capability flag
  isolates this risk — if it turns out unsupported, flip opencode to
  `FirstTurn` with no other change.
