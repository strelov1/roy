# Persistence and resume

## Workspace layout

All project and orphan-session working directories live under
`workspace_dir` (default `~/.roy/workspace/`; override via `ROY_WORKSPACE`
env var or `roy serve --workspace-dir <path>`):

```
<workspace_dir>/
  <project_name>/      # one per project; created at CreateProject
  <session_id>/        # one per orphan session; created at spawn-without-project
```

The daemon creates these directories but does not own their contents after
creation — the agent and the user write to them freely.

**Cascade delete of a project removes the registry entry and each session's
`.jsonl` / `.meta.json` files, but does NOT remove `<workspace>/<name>/`.
The user may have committed work in that directory.**

## Project registry

`<journal_dir>/projects.json` lists every project:

```json
{
  "version": 1,
  "projects": [
    {
      "id": "1f7c…",
      "name": "roy",
      "path": "/Users/alice/.roy/workspace/roy",
      "created_at": 1722345600
    }
  ]
}
```

Written atomically (temp file in same directory + `rename`) after every
mutation. Missing `version` is treated as `v1`. Unknown `version` is a
hard error.

At startup, `index_existing_sessions` scans all `.meta.json` files and
rebuilds `sessions_by_project`. If a meta references a `project_id` that
is not in the registry, the session is logged as a warning and skipped
from the index — no auto-create. The user must clean up by hand (delete
the `.jsonl` / `.meta.json` pair or restore `projects.json`).

---

Each session writes two files under `journal_dir` (defaults to
`~/.roy/journals/`):

```
<session_id>.jsonl       — append-only event log
<session_id>.meta.json   — sidecar metadata, rewritten on cursor change
```

Both survive daemon restarts. Together they make a session
**resurrectable**: a fresh `roy serve` process can rebuild a live
`SessionEngine` from disk without losing journal continuity.

## Journal file

One `JournalEntry` per line, JSONL:

```jsonl
{"seq":0,"event":{"type":"system","subtype":"init"}}
{"seq":1,"event":{"type":"assistant_text","text":"…"}}
{"seq":2,"event":{"type":"tool_use","name":"Bash","input":{"command":"ls"}}}
{"seq":3,"event":{"type":"result","cost_usd":null,"stop_reason":"end_turn","is_error":false}}
```

- `seq` is monotonic across all turns of a session.
- Resumed sessions continue past the last persisted seq (the
  `Journal::resume` constructor re-reads the file tail to recover
  `next_seq`).
- A turn always ends with a `result` entry. If the transport dies
  mid-turn, the engine synthesises
  `result { stop_reason: "error" }` so the on-disk log is still a valid
  sequence of turns.

The file is opened with `O_APPEND` and `flush`ed after every line. The
in-memory ring window in `Journal` is an optimisation for fast
`replay_from` near the tail — the disk file is always the source of
truth.

`tail -f <session_id>.jsonl` is a valid observation tool because the
on-disk format is exactly the same JSON shape that goes onto CLI stdout
and into trigger frames.

## Metadata file

JSON, rewritten atomically (temp file + `rename`) every time the
session's `resume_cursor` changes:

```json
{
  "session_id": "0a91…",
  "agent": "opencode",
  "cwd": "/Users/alice/.roy/workspace/myproject",
  "model": null,
  "permission": "allow",
  "project_id": "1f7c…",
  "resume_cursor": "sess_abc123"
}
```

Fields:

| field           | source                                                         |
|-----------------|-----------------------------------------------------------------|
| `session_id`    | roy-side UUID minted at first spawn; stable across restarts     |
| `agent`         | the preset name (`claude`, `gemini`, `opencode`, `codex`)       |
| `cwd`           | the working directory for this session                          |
| `model`         | the `--model` flag, if applicable (claude only)                 |
| `permission`    | the requested `PermissionPolicy` (`allow` / `deny`)             |
| `project_id`    | UUID of the owning project; `null` means orphan session         |
| `resume_cursor` | the agent-issued session id (e.g. ACP `sessionId`) most recently observed from `Handle::resume_cursor()` |

`cwd` is kept on `SessionMetadata` even though it mirrors the project path,
so meta files remain self-contained (no registry lookup needed to interpret
one file in isolation).

The atomic write is a temp file inside the same directory plus
`tokio::fs::rename` — partial writes never replace the canonical file,
so a crash mid-write leaves the previous valid metadata intact.

## Two ids: roy-side vs agent-side

These are easy to confuse but kept strictly separate:

- **`session_id` (roy-side)** — a UUID minted by roy at first spawn.
  Stable forever for that session. This is what you pass to
  `roy attach`, `roy close`, `roy resume`, etc.
- **`resume_cursor` (agent-side)** — opaque token issued by the agent
  (for ACP, it's the `sessionId` returned by `session/new` and accepted
  by `session/load`). roy persists it but never tries to interpret it.

At resume time, the roy-side id and journal stay the same; only the
agent-side cursor is replayed into `Transport::open`.

## Resume flow

```
┌─ on disk ─────────────────┐         ┌─ live ─────────────────────┐
│  <id>.jsonl   (history)   │         │  SessionEngine             │
│  <id>.meta.json (cursor)  │ ──────► │   reads cursor → passes to │
│                            │ resume  │   Transport::open ──► ACP  │
│                            │         │     session/load           │
└────────────────────────────┘         └────────────────────────────┘
```

Triggered by either:

- `roy resume <session_id>` — explicit one-session resurrect.
- `roy serve --resume-all` — daemon scans `journal_dir` at startup and
  brings back every archived session.

What survives:

| thing                    | survives restart? | how                                        |
|--------------------------|--------------------|--------------------------------------------|
| roy session id           | yes                | persisted as the journal filename          |
| journal contents         | yes                | append-only file on disk                   |
| `resume_cursor`          | yes                | persisted in `.meta.json`                  |
| agent process            | **no**             | killed with the previous daemon            |
| in-memory broadcast      | no                 | bounded ring, rebuilt empty on resume      |
| input lease state        | no                 | resets to "no holder" on resume            |

What the agent itself remembers depends on the agent. Gemini and
OpenCode persist their session and continue exactly where they left
off after `session/load`. Other agents may treat `session/load` as
"please start fresh" — in that case the roy-side journal still
continues monotonically, but the agent has no memory of the prior
conversation.

## Read-only access without resuming

Two paths to inspect a closed session:

1. `tail -f <journal_dir>/<id>.jsonl` — the on-disk format is the
   public wire format.
2. `roy attach <id>` — if `<id>` isn't live, the daemon falls back to a
   read-only archive replay (`ArchivedJournal::replay_from`) and
   streams the journal as `Frame` events. The stream ends after the
   last on-disk entry; no new events will arrive until/unless someone
   resumes the session.

`roy list-archived` shows session ids whose journals exist on disk but
whose engines are not currently live — survivors of daemon restarts
plus sessions that were explicitly closed.

## Idle GC

When `roy serve --idle-timeout <seconds>` is set, a background ticker
calls `SessionManager::sweep_idle(threshold)` at
`max(threshold / 4, 50ms)` intervals. Any session whose
`last_activity` is older than the threshold is closed (its journal +
metadata remain on disk and are still resurrectable).

"Activity" is defined as either:

- a new `JournalEntry` was appended (the agent produced output), or
- a `Cmd::Prompt` arrived at the actor (so a slow agent still being
  primed doesn't get GC'd before it streams).

Pure observers (`attach`) do **not** count as activity — a session
with subscribers but no agent traffic still ages out.
