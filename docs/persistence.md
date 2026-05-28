# Persistence and resume

## Database layout

Session boot-kit and project metadata are now split across two SQLite databases.

**Core sessions** (`~/.local/state/roy/sessions.db`):
- `sessions` table: `session_id` (PK), `agent`, `cwd`, `model`, `permission`,
  `resume_cursor`, `system_prompt`, `created_at`, `closed_at`.
- Owned by `roy::SessionStore`. Written during spawn and cursor updates.
- Each row is resurrectable: the daemon can rebuild a live `SessionEngine`
  from the boot-kit columns without losing journal continuity.

**Management metadata** (`~/.local/state/roy/agents.db`):
- `projects` table: `id` (PK), `name` (unique), `path`, `created_at`.
- `session_meta` table: `session_id` (PK, FK to sessions), `project_id`,
  `agent_id`, `agent_name`, `display_label`.
- `session_tags` table: `session_id`, `key`, `value` (composite PK).
- Owned by `roy-management::MetaStore`, co-located with `roy-agents`'s
  `agents` table.
- Written by HTTP API (`POST /sessions`, `PUT /sessions/{id}/tags`, etc.).
- Migrations: both databases use SQLite `_sqlx_migrations` with
  `set_ignore_missing(true)` so partial updates don't fail. `roy-agents`
  owns v1 of agents.db migrations; `roy-management` adds v2 for the three
  new tables. `sessions.db` has its own migration track.

---

Each session writes one file under `journal_dir` (defaults to
`~/.roy/journals/`):

```
<session_id>.jsonl       — append-only event log
```

This survives daemon restarts. Together with the boot-kit row in
`sessions.db`, it makes a session **resurrectable**: a fresh `roy serve`
process can rebuild a live `SessionEngine` from disk without losing journal
continuity.

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

## Boot-kit row (sessions.db)

SQLite row in the `sessions` table, updated atomically each time the
session's `resume_cursor` changes:

| column          | type        | source                                                         |
|-----------------|-------------|------------------------------------------------------------------------|
| `session_id`    | text PK     | roy-side UUID minted at first spawn; stable across restarts     |
| `harness`       | text        | the harness name (`claude`, `gemini`, `opencode`, `codex`, `pi`) |
| `cwd`           | text        | the working directory for this session                          |
| `model`         | text        | the `--model` flag, if applicable (claude only); null if unset  |
| `permission`    | text        | the requested `PermissionPolicy` (`allow` / `deny`)             |
| `resume_cursor` | text        | the agent-issued session id (e.g. ACP `sessionId`) most recently observed |
| `system_prompt` | text        | snapshot of the inline persona prompt; re-applied on `resume`. null when none was set |
| `created_at`    | integer     | unix timestamp of spawn time                                    |
| `closed_at`     | integer     | unix timestamp of close time; null while live                   |

The SQLite transaction is atomic — partial writes never leave the database
in an inconsistent state.

## Enriched metadata (agents.db)

Project and session-level rich metadata live in separate tables, joined at
query time via HTTP APIs in `roy-management`:

**projects**: `id`, `name` (unique), `path`, `created_at`.

**session_meta**: per-session enrichment — `session_id` (FK to sessions.db),
`project_id` (FK to projects), `agent_id`, `agent_name`, `display_label`.
Allows sessions to be tagged with a project and display label without
mutating the immutable boot-kit.

**session_tags**: key-value tags — `session_id`, `key`, `value` (composite
PK). Queryable and editable via HTTP APIs for organizing sessions.

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
┌─ on disk ──────────────────┐         ┌─ live ─────────────────────┐
│  sessions.db (boot-kit)    │         │  SessionEngine             │
│   ├ session_id (PK)        │         │   reads boot-kit row       │
│   ├ agent, cwd, model      │ ─────► │   → passes to              │
│   ├ resume_cursor          │ resume  │   Transport::open ──► ACP  │
│   └ ...                    │         │     session/load           │
│  <id>.jsonl (history)      │         │                            │
└────────────────────────────┘         └────────────────────────────┘
```

Triggered by either:

- `roy resume <session_id>` — explicit one-session resurrect.
- `roy serve --resume-all` — daemon queries `sessions.db` at startup and
  brings back every session with `closed_at IS NULL`.

What survives:

| thing                    | survives restart? | how                                        |
|--------------------------|--------------------|--------------------------------------------|
| roy session id           | yes                | persisted in `sessions.session_id` (PK)   |
| journal contents         | yes                | append-only JSONL file on disk             |
| boot-kit (agent, cwd)    | yes                | persisted in `sessions` row                |
| `resume_cursor`          | yes                | persisted in `sessions.resume_cursor`      |
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
