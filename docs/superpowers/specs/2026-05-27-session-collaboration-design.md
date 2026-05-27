# Session-to-session collaboration: `roy ask` + `ROY_SESSION_ID`

## Motivation

A background agent (typically launched via `roy-scheduler` fire or ad-hoc `roy
run`) routinely hits points where it needs an external decision: "should I
proceed with X or Y", "is this finding worth acting on", "which of these two
matches did you mean". Today the agent's only outbound channel is
`roy inject <session>` (added in
`2026-05-25-agent-driven-inject-design.md`): a one-way `Note` into the target
session's journal. There is no way to **wait** for a decision and continue —
the agent must finish its turn, leaving the work suspended until the user
manually re-fires it.

We want the agent to be able to:

1. **Ask a human** for input by dropping a question into the human's main
   session and continuing once the human responds.
2. **Ask another agent** synchronously and use the returned text to keep
   going (CrewAI-style `AskQuestionTool` / `DelegateWorkTool`).

CrewAI confirms the natural shape: a single primitive — *"synchronously
invoke another agent with prompt+context, get text back"* — covers both ask
and delegate framings; the two surface tools differ only in LLM-facing
description, not mechanics
(`lib/crewai/src/crewai/tools/agent_tools/{ask_question_tool.py,delegate_work_tool.py}`
in `crewAIInc/crewAI` both call the same `BaseAgentTool._execute → selected_agent.execute_task(...)`).

In roy, that primitive **already exists**: `ClientCommand::Fire { target,
prompt, timeout_ms }` (`crates/roy/src/control.rs:213`) spawns or resumes a
session, sends the prompt, blocks on the terminal `Result`, returns the
assistant text. And the human-side return path is already wired:
`MessageGroups.svelte:317-337` in `roy-web` renders a `Note` with a
clickable link to `source_session` that calls `app.openSession(src)`.

This spec adds the **thin wrappers** that turn those existing primitives
into a useful agent tool, plus the one missing piece the agent needs to
self-identify on the human-decider path.

## What we add

### 1. `roy ask` CLI subcommand (`crates/roy-cli`)

```
roy ask <target> "<prompt>" [--context "<ctx>"] [--timeout 10m]
```

- `<target>` is resolved in this order:
  1. **Live session id** — if a session with that id is currently live
     (`ClientCommand::List` returns it), use `Fire { target:
     FireTarget::Resume { session_id } }`.
  2. **Agent slug / id** — otherwise look it up via
     `ManagementClient::resolve(target)` (existing in
     `crates/roy-cli/src/management_client.rs:188`) and on hit, fetch the
     agent's `preset` + `prompt` and call `Fire { target: FireTarget::Spawn
     { preset, system_prompt: Some(agent.prompt) } }`.
  3. Otherwise exit 2 with `unknown target: <target>` on stderr.
- The effective prompt sent is `prompt` if `--context` is absent, else
  `format!("Context:\n{ctx}\n\nQuestion/Task:\n{prompt}")` — mirrors
  CrewAI's two-field `(task, context)` shape.
- `--timeout` accepts the same syntax as elsewhere in roy-cli (default
  `10m`, matches `Fire`'s default).
- **Stdout**: a single JSON line — `{"type":"answer","session":"...","text":"..."}`
  on success. The `text` field carries `FireDone.assistant_text`
  (`control.rs:344`) — the accumulated `AssistantText` between the prompt
  and the terminal `Result`. Error/timeout outcomes don't print a JSON
  line; structured stderr explains and the exit code differentiates.
- **Stderr**: structured logs via `init_tracing` (same as other
  subcommands).
- **Exit codes**:
  - `0` — `FireDone` (terminal `Result`, non-error stop reason).
  - `1` — `FireDone` with `Result.stop_reason.is_error()`.
  - `2` — CLI-level failure: unknown target, `FireTimeout`, `FireError`,
    daemon unreachable, transport hang-up.

`roy delegate` is **not** added in this spec. Two surface verbs is a
prompt-engineering nicety, not a code concern; if it becomes useful later
it's a one-line alias in clap. We keep the surface minimal until there's a
concrete need.

### 2. `ROY_SESSION_ID` env var at child spawn (`crates/roy/src/transport/acp/mod.rs`)

In `AcpTransport::open` (currently `_session_id: &str` at line 185), drop
the `_` prefix and add one line before `cmd.spawn()`:

```rust
cmd.env("ROY_SESSION_ID", session_id);
```

This lets a background agent's Bash tool reference its own roy session id
without the orchestrator having to template it into the prompt. Used for
`roy inject <human> "..." --source $ROY_SESSION_ID` (the human-decider
path) and, optionally, by the agent's prompt template for context.

No behaviour change for existing flows — the variable is purely additive.

### 3. roy-web: visual marker for "ask"-style notes (`../roy-web/src/lib/components/MessageGroups.svelte`)

Purely cosmetic. When a `Note.text` begins with `[ask]` (the convention
`roy ask` itself does not enforce — it lives in the prompt template the
agent uses for the human path), change the chip label from `background` to
`background · awaiting reply` and switch the border color from
`border-primary/30` to `border-amber-400/40`. No new fields on the wire,
no new event variants.

This affordance is best-effort; the link to `source_session` is the real
mechanism and already works.

## What we do not add

- **New `ClientCommand` / `ServerEvent` / `TurnEvent` variants.** All the
  routing the spec covers already has wire-level primitives.
- **Long-poll watching another session's journal for user-prompts** (the
  earlier "Approach 1" in brainstorming). Out — covered by `Fire`'s
  existing `WaitForResult` semantics for the agent-decider path and by the
  Note-link UX for the human-decider path.
- **Async re-fire / "background sleeps until human answers"**. The human
  path is sync from the agent's perspective (its turn finishes after the
  `inject`, the human reply re-prompts the same session) and stateless from
  the daemon's perspective (no pending-question table). The agent-decider
  path holds the asker's turn open for the duration of the callee's `Fire`,
  which is what `Fire` already does today.
- **Persona-name disambiguation when slug collides with a live session
  id.** Live-session-id check runs first; collision in practice is near
  zero (UUIDs vs short slugs). Spec'd order, not auto-detected.
- **Concurrent asks to the same target.** Two asker sessions hitting the
  same target via `Fire { Resume }` serialize on the engine's input lease
  (`engine.rs:407` `try_acquire_input`); the second one currently gets
  `NoLease` from `Fire`. Not addressed here — same behavior as `Fire`
  today; queueing is a separate problem.

## Data flow

### Human-decider path

```
background-agent (session SB, knows ROY_SESSION_ID=SB from its env)
  ↓ runs in its turn:
  $ roy inject <SH> "Found two candidates: A or B. Which?" --source $ROY_SESSION_ID
      → daemon: ClientCommand::Inject { session: SH, text, source_session: Some(SB) }
      → SessionEngine(SH).inject_note(...)  [no lease, no transport]
      → Note appended to SH.journal + broadcast; SH's ACP agent does NOT see it
  ↓ background-agent's turn completes; SB is now idle

human (attached to SH in roy-web)
  ↓ sees Note rendered by MessageGroups.svelte with link "background · SB.."
  ↓ clicks link → app.openSession(SB) → roy-web switches to SB
  ↓ types "Pick A" into SB's composer
      → ClientCommand::Spawn-or-resume + Cmd::Prompt to SB's ACP agent
  ↓ background-agent receives "Pick A" as a new turn, continues from there
```

### Agent-decider path

```
background-agent (session SB)
  ↓ runs in its turn:
  $ roy ask reviewer "Is this finding worth a PR?" --context "<paste of finding>"
      → roy-cli resolves target:
          - List() does not return "reviewer" as a live session id;
          - ManagementClient::resolve("reviewer") finds an agent with that slug;
          - fetch its preset + prompt.
      → ClientCommand::Fire {
            target: Spawn { preset, system_prompt: Some(reviewer.prompt) },
            prompt: "Context:\n...\n\nQuestion/Task:\n Is this finding worth a PR?",
            timeout_ms: 600_000,
        }
      → daemon spawns a new session SR, runs the turn, waits for terminal Result.
      → ServerEvent::FireDone { session: SR, assistant_text: "Yes, because ...", result, seq_range }
  ↓ roy ask prints {"type":"answer","session":"SR","text":"Yes, because ..."}
  ↓ background-agent's Bash tool returns that text to the LLM, continues.
```

## Testing

- `roy-cli` unit / integration:
  - Target resolution: live session id → `Fire { Resume }`; agent slug →
    `Fire { Spawn { preset, system_prompt } }`; unknown → exit 2.
  - Context concatenation produces the documented "Context:\n.../\nQuestion/Task:\n..."
    shape.
  - Exit codes: 0 on `FireDone` non-error; 1 on `FireDone` with
    `is_error()`; 2 on `FireTimeout` / `FireError` / unknown target.
  - Daemon-backed test (reuse the harness in `crates/roy/src/daemon.rs`
    tests) where `roy ask <live_session_id> "..."` yields `FireDone` from
    the fake agent and prints the expected JSON line.
- `crates/roy/src/transport/acp/mod.rs`:
  - The fake ACP agent script (`tests/scripts/fake-acp-agent.py`) is
    extended to echo its `ROY_SESSION_ID` into a log file or first turn's
    text; a new integration test asserts the variable is set to the host
    session id.
- `roy-web`:
  - Snapshot / DOM test (whatever roy-web uses today) for the `[ask]`
    prefix → amber chip rendering. Skip if roy-web has no such harness;
    visual change is small.
- Whole workspace: `cargo fmt --all -- --check`, `cargo build --workspace
  --all-targets`, `cargo test --workspace --no-fail-fast` all green.

## Docs

- `docs/wire-protocol.md` — no change (no new wire variants).
- `crates/roy/CLAUDE.md` — one paragraph under a new
  "Session-to-session collaboration" heading describing the two patterns
  and pointing at `roy ask` + `ROY_SESSION_ID`.
- Background-agents skill doc (lives in `~/.claude/skills/`, separate
  repo) — follow-up note, not part of this change.

## Out of scope (deferred)

- `roy delegate` as a second LLM-facing verb — add when a concrete
  agent-prompt regression motivates it.
- Async re-fire when a background must wait hours for a human reply.
- A "decision-making peer" persona pre-bundled in `agents.toml`.
- Bot-style hierarchical orchestration (CrewAI's manager-agent mode).
- Queueing concurrent `Fire { Resume }` against the same target.
