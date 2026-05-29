# Persistence and resume

`roy` keeps state in three SQLite files plus a per-session JSONL journal
on disk. The journal is the public on-disk wire format (same JSON shape as
CLI stdout and WS frames); the SQLite files are private to their owner
crates.

## Files

| Path                                            | Owner                                  | Tables                                                              |
|-------------------------------------------------|----------------------------------------|---------------------------------------------------------------------|
| `~/.local/state/roy/sessions.db`                | `roy::SessionStore`                    | `sessions`                                                          |
| `~/.local/state/roy/agents.db`                  | `roy-management` + `roy-auth`          | `projects`, `session_meta`, `session_tags`, `connections`, `users`, `teams`, `team_members`, `team_invites` |
| `~/.local/state/roy-scheduler/state.db`         | `roy-scheduler` (writer); `roy-management` reads only | `agents`, `triggers`, `fires`, `fire_subscribers`, `fire_subscriber_runs` |
| `~/.local/state/roy-inbound/state.db`           | `roy-inbound`                          | `bindings`                                                          |
| `<journal_dir>/<session_id>.jsonl`              | `roy::Journal`                         | append-only JSONL event log (defaults to `~/.roy/journals/`)        |

All SQLite files are opened with WAL mode, foreign keys on, busy timeout
5 s, and chmod 0600 on Unix.

`agents.db` is shared by `roy-management` and `roy-auth`. The two crates'
migrations live side-by-side in the shared `_sqlx_migrations` table;
each migrator runs with `set_ignore_missing(true)` so it tolerates rows
owned by the other. SQLite allows DDL forward references, so the
roy-management table that declares `created_by REFERENCES users(id)` can
be created before `users` exists — the FK is enforced only at DML time,
by which point `bootstrap::ensure_root` has already populated `users`.

## Journal file

One `JournalEntry` per line, JSONL:

```jsonl
{"seq":0,"event":{"type":"system","subtype":"init"}}
{"seq":1,"event":{"type":"user_prompt","text":"…"}}
{"seq":2,"event":{"type":"assistant_text","text":"…"}}
{"seq":3,"event":{"type":"tool_use","name":"Bash","input":{"command":"ls"}}}
{"seq":4,"event":{"type":"result","cost_usd":null,"stop_reason":"end_turn","is_error":false}}
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

## `sessions` boot-kit (`sessions.db`)

| column          | type        | source                                                                       |
|-----------------|-------------|-------------------------------------------------------------------------------|
| `session_id`    | text PK     | roy-side UUID minted at first spawn; stable across restarts                   |
| `harness`       | text        | the harness name (`claude`, `gemini`, `opencode`, `codex`, `pi`)              |
| `cwd`           | text        | the working directory for this session                                        |
| `model`         | text        | the `--model` flag, if applicable; null if unset                              |
| `permission`    | text        | the requested `PermissionPolicy` (`allow` / `deny`)                            |
| `resume_cursor` | text        | the agent-issued session id (e.g. ACP `sessionId`) most recently observed     |
| `system_prompt` | text        | snapshot of the inline persona prompt; re-applied on `resume`. null when none |
| `created_at`    | integer     | unix timestamp of spawn time                                                  |
| `closed_at`     | integer     | unix timestamp of close time; null while live                                 |

Updated atomically each time `resume_cursor` changes.

## Management metadata (`agents.db`)

Project- and session-level enrichment, joined with `sessions.db` on
`session_id` at query time via HTTP APIs in `roy-management`.

- **projects**: `id`, `name` (unique), `path`, `created_by` (FK
  `users(id)`), optional `team_id` (FK `teams(id)`), `created_at`.
- **session_meta**: `session_id` (PK, soft FK to `sessions.db`),
  optional `project_id`, `agent_id`, `agent_name`, `display_label`,
  `created_by`, optional `team_id`, `connection_ids` (JSON array, audit
  only — resume gets a clean MCP slate), `created_at`.
- **session_tags**: `session_id` (FK `session_meta` ON DELETE CASCADE),
  `key`, `value` (composite PK).
- **connections**: per-user MCP-server bindings.
  Composite uniqueness `(owner_id, slug)`; partial unique
  `(owner_id, provider_id, name)` when `provider_id IS NOT NULL`,
  so each user can have one labelled instance per catalog provider.
  Secrets live inline in `secrets_json` (`agents.db` is `0600`).
- **channel_bindings**: bot→agent bindings for inbound channels.
  Columns: `id`, `owner_id` (FK `users`), `channel_kind` (e.g.
  `telegram`), `connection_id` (FK `connections` ON DELETE CASCADE),
  `agent_slug`, `agent_scope` (`"user"` or `"team:<id>"`),
  `session_strategy` (`ephemeral` / `persistent_one` /
  `per_sender_sticky`), `idle_timeout_secs` (nullable), `allowed_user_ids`
  (JSON array, nullable), `enabled`, `created_at`, `updated_at`.
  `UNIQUE(connection_id)` — one binding per bot.
  Owned by `roy-management` (CRUD via the HTTP API); read by
  `roy-inbound` through the internal HTTP endpoint
  `GET /internal/telegram-sources` (not direct DB access).

## Auth tables (`agents.db`)

- **users**: `id`, `username` (UNIQUE COLLATE NOCASE), `display_name`,
  `password_hash`, optional `timezone`, `created_at`.
- **teams**: `id`, `name`, `description`, optional `created_by` (FK
  `users`), `created_at`.
- **team_members**: `(user_id, team_id)` PK, `role` (default `'member'`),
  `joined_at`.
- **team_invites**: `token` (PK), `team_id` (FK), `created_by` (FK),
  `created_at`, optional `expires_at`, optional `accepted_by`, optional
  `accepted_at`.

## Scheduler state (`roy-scheduler/state.db`)

- **agents**: registered agents (`id`, `name`, `harness`, optional
  `project_id`, `task`, optional `model`, `persistent` flag, optional
  `persistent_session_id`, optional `notify_session`).
- **triggers**: cron or one-shot fires (`kind IN ('cron','oneshot')`,
  `cron_expr`/`fire_at`, `next_fire_at`, `paused`).
- **fires**: per-fire audit row (`status IN ('running','ok','error',
  'timeout')`, `transcript_seq_range_*`, `assistant_text`, `cost_usd`,
  `stop_reason`).
- **fire_subscribers**: registered post-fire effects (`kind IN
  ('inject_parent','webhook','notify_native','chain_agent')`, JSON
  `config`, `enabled`, `order_index`). Either `agent_id` or
  `trigger_id` is required.
- **fire_subscriber_runs**: per-subscriber audit row.

The Postgres dialect of the same schema lives in
`crates/roy-scheduler/migrations/postgres/`.

The roy-scheduler binary owns this file: it creates it and runs the
migrations. `roy-management` opens it **read-only via the scheduler's read
facade (`roy_scheduler::read::SchedulerRead` over `db::open_read_only`) and
never migrates it** — it only surfaces `agents`/`triggers`/`fires` on its HTTP
read endpoints. The shared WAL file is fine because exactly one process (the
scheduler) ever writes or migrates.

## Inbound bindings (`roy-inbound/state.db`)

- **bindings**: `(source_id, sender_id)` UNIQUE → `session_id`,
  `agent_id`, `strategy`, `created_at`, `last_active_at`. Sticky
  per-sender mappings for `per_sender_sticky` strategy survive process
  restarts.

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
│   ├ harness, cwd, model    │ ─────► │   → passes to              │
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
| roy session id           | yes                | persisted in `sessions.session_id` (PK)    |
| journal contents         | yes                | append-only JSONL file on disk             |
| boot-kit (harness, cwd)  | yes                | persisted in `sessions` row                |
| `resume_cursor`          | yes                | persisted in `sessions.resume_cursor`      |
| agent process            | **no**             | killed with the previous daemon            |
| in-memory broadcast      | no                 | bounded ring, rebuilt empty on resume      |
| input lease state        | no                 | resets to "no holder" on resume            |
| attached MCP connections | no                 | snapshot only, not re-attached on resume   |

What the agent itself remembers depends on the harness. Gemini and
OpenCode persist their session and continue exactly where they left
off after `session/load`. Other harnesses may treat `session/load` as
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
