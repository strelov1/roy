# Background agents — design spec

**Status:** draft, awaiting implementation plan
**Date:** 2026-05-23
**Scope:** v1 of background-agent / scheduled-fires for the `roy` stack.

## 1. Goal

Give `roy` first-class support for **persistent background agents** that run
on schedule (cron) or on demand, persist their identity and memory across
runs, and dispatch their results to one or more **subscribers** (inject into a
parent session, POST to a webhook, raise a native notification).

This already exists in `~/Projects/claude-agent` as a Next.js + Postgres
monolith, hardcoded to Claude. The goal here is to lift the pattern into the
multi-agent `roy` stack (`claude` / `gemini` / `opencode` / `codex`) without
dragging in the Next.js, auth, teams, projects, or UI baggage.

### Non-goals (v1, explicit)

- Web UI of any kind. CLI only.
- Multi-user / multi-tenant. Single-user, single-machine.
- Postgres. SQLite first; PG-readiness is built in (see §3.4) but not shipped.
- `chain_agent` subscriber (one fire triggers another agent). Placeholder in
  the schema enum, but rejected in code — needs loop-detection design.
- Webhook retry policy beyond one attempt.
- Secret encryption for webhook headers (tokens). Plain JSON; the SQLite file
  is `0600`.
- Skills/persona injection à la claude-agent (`agent_slug` → hook). The
  agent's `task` field **is** the prompt. Persona, if wanted, goes inline.
- Multi-instance scheduler. PidLock-guarded single instance only.

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│ user / MCP-client / roy-scheduler                                │
└──────────────────────────┬───────────────────────────────────────┘
                           │ control-protocol JSON (UDS / WS)
                           │ (same wire as roy run / MCP)
                           ▼
┌──────────────────────────────────────────────────────────────────┐
│ roy daemon (crates/roy)                                          │
│   + Session tags  (BTreeMap<String,String> on SessionMetadata)   │
│   + WaitForResult { session, since_seq, timeout_ms }   long-poll │
│   + Fire { target, prompt, tags, timeout_ms }          combo     │
└──────────────────────────────────────────────────────────────────┘
                           ▲
                           │ control-protocol (as an ordinary client)
                           │
┌──────────────────────────────────────────────────────────────────┐
│ roy-scheduler (crates/roy-scheduler)                             │
│   - SQLite store (agents/triggers/fires/subscribers/sub_runs)    │
│   - planTick                 — pure function, port from TS       │
│   - driver                   — poll-loop + claim + invoke_fire   │
│   - protocol client          — talks to roy daemon over UDS/WS   │
│                                                                  │
│   bin: roy-scheduler                                             │
│     serve | agents | triggers | subscribers | fires | fire-now   │
└──────────────────────────────────────────────────────────────────┘
```

### 2.1 Boundary rule

`roy-scheduler` may import from `roy` **only the `protocol` module** (the
wire-level enums: `ClientCommand` / `ServerEvent` / `TurnEvent` /
`SessionMetadata` / `TagMap`). It must not depend on `Daemon`,
`SessionManager`, `SessionEngine`, `Transport`, or `Journal` directly.

The test of whether the boundary holds: `cargo test -p roy-scheduler` must
work against a **mock roy daemon** (a `tokio::io::duplex` that scripts WS/UDS
JSON replies), with no link to roy internals. This is the contract; CLAUDE.md
gets a short note about it.

### 2.2 Why not isolate into a separate repo

Two repos buy a release-cycle separation we don't need, force a third
`roy-protocol` crate or a hand-maintained copy of wire types, and still
require the scheduler to cross the wire boundary on every call (no in-process
shortcut). The single-workspace layout with a publicly-enforced import rule
gives the same isolation guarantee at a fraction of the operational cost.
Extraction later is a `git mv` because the boundary is already wire-shaped.

## 3. Changes to `roy`

Three small additions. All three are independent: each ships with its own
tests and is useful on its own (not just for the scheduler).

### 3.1 Session tags

Add a sorted key/value map to session metadata:

```rust
// crates/roy/src/manager.rs (or wherever SessionMetadata lives)
pub struct SessionMetadata {
    // ...existing fields...
    pub tags: BTreeMap<String, String>,   // sorted → deterministic JSONL
}
```

Wire surface:

- `ClientCommand::Run { ..., tags: Option<BTreeMap<String,String>> }` — set on
  spawn.
- `ClientCommand::Resume { ..., tags: Option<BTreeMap<String,String>> }` —
  `None` leaves the existing tag map untouched. `Some(map)` upserts each key
  (existing keys overwritten, unmentioned keys left alone). To delete a tag,
  use `SetTags` with a map that omits it.
- `ClientCommand::SetTags { session_id, tags }` — replace the live session's
  tag map; emits `ServerEvent::SessionUpdated`.
- `ServerEvent::SessionInfo` (used by `list` / `list-archived`) — includes
  `tags`.
- Persisted in the on-disk `SessionMetadata` file, survives daemon restarts.

**Roy never interprets tag values.** Reserved key prefix for the scheduler is
`roy-scheduler:`. The scheduler writes these on every fire:

| Key                                    | Value                       |
|----------------------------------------|-----------------------------|
| `roy-scheduler:agent_id`               | agent ulid                  |
| `roy-scheduler:trigger_id`             | trigger ulid (omitted on ad-hoc fires) |
| `roy-scheduler:fire_id`                | fire ulid                   |
| `roy-scheduler:parent_session_id`      | parent roy session id (omitted if no `inject_parent` subscriber) |
| `roy-scheduler:kind`                   | `background_fire`           |

Other layers pick their own prefixes; there is no central registry.

### 3.2 `WaitForResult` long-poll

```rust
ClientCommand::WaitForResult {
    session_id: SessionId,
    since_seq:  Option<u64>,   // default 0 = "wait for the next Result after now"
    timeout_ms: Option<u64>,   // default 600_000 (10 min)
}

ServerEvent::ResultReady {
    session_id: SessionId,
    seq: u64,
    result: TurnEvent,         // the terminal Result variant
    assistant_text: String,    // concatenated AssistantText entries from (since_seq..=seq)
}
ServerEvent::WaitTimeout { session_id: SessionId }
```

Semantics: resolve when the next `TurnEvent::Result` with `seq >= since_seq`
lands in the session's journal. Backed by the existing per-engine
`broadcast::Sender` plus a journal scan to cover the race where the Result
already happened before the subscription.

Lets the scheduler track turn completion without holding a long-running
`attach`.

### 3.3 `Fire` — combo command

```rust
pub enum FireTarget {
    Spawn  { preset: String, cwd: PathBuf },
    Resume { session_id: SessionId },
}

ClientCommand::Fire {
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
    timeout_ms: Option<u64>,                  // default 600_000 (10 min), same as WaitForResult
}

ServerEvent::FireDone {
    session_id: SessionId,
    seq_range: (u64, u64),
    result: TurnEvent,           // terminal Result
    assistant_text: String,
}
ServerEvent::FireTimeout { session_id: SessionId, partial_seq_range: (u64, u64) }
ServerEvent::FireError   { session_id: Option<SessionId>, code: ErrorCode, message: String }
```

Under the hood: `Run` (or `Resume`) → `WaitForResult` → respond. This is the
99% scheduler call: one round trip instead of two, and one shot at error
handling.

`Run` + `WaitForResult` stay public for the rare case where a caller wants to
stream events while waiting or branch behaviour mid-turn.

## 4. `roy-scheduler` — SQLite schema

File: `~/.local/state/roy-scheduler/state.db` by default (overridable via
`ROY_SCHEDULER_DB`). Created with mode `0600`. WAL mode,
`busy_timeout=5s`.

```sql
-- A persistent identity. The "sotrudnik".
CREATE TABLE agents (
  id                       TEXT PRIMARY KEY,                       -- ulid
  name                     TEXT NOT NULL,
  preset                   TEXT NOT NULL,                          -- claude | gemini | opencode | codex
  cwd                      TEXT NOT NULL,
  task                     TEXT NOT NULL,                          -- prompt template sent as the user turn
  model                    TEXT,                                   -- optional preset-specific override
  persistent               INTEGER NOT NULL DEFAULT 0,             -- 0/1; fires reuse one child session if 1
  persistent_session_id    TEXT,                                   -- roy session id, set on the first persistent fire
  created_at               TEXT NOT NULL,                          -- ISO-8601 UTC
  updated_at               TEXT NOT NULL
);

-- When to fire.
CREATE TABLE triggers (
  id              TEXT PRIMARY KEY,
  agent_id        TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  kind            TEXT NOT NULL CHECK(kind IN ('cron','oneshot')),
  cron_expr       TEXT,                                            -- required when kind='cron'
  timezone        TEXT NOT NULL DEFAULT 'UTC',                     -- overrides ROY_SCHEDULER_TZ for this trigger
  fire_at         TEXT,                                            -- required when kind='oneshot'
  next_fire_at    TEXT NOT NULL,                                   -- always populated
  last_fire_at    TEXT,
  paused          INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  created_at      TEXT NOT NULL
);
CREATE INDEX triggers_due_idx ON triggers(paused, next_fire_at);

-- One row per fire attempt.
CREATE TABLE fires (
  id                          TEXT PRIMARY KEY,
  agent_id                    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  trigger_id                  TEXT REFERENCES triggers(id) ON DELETE SET NULL,  -- null = ad-hoc
  session_id                  TEXT,                                -- roy session id, set as soon as roy returns one
  status                      TEXT NOT NULL CHECK(status IN ('running','ok','error','timeout')),
  started_at                  TEXT NOT NULL,
  finished_at                 TEXT,
  transcript_seq_range_start  INTEGER,
  transcript_seq_range_end    INTEGER,
  assistant_text              TEXT,
  cost_usd                    REAL,
  stop_reason                 TEXT,
  error_message               TEXT
);
CREATE INDEX fires_agent_idx ON fires(agent_id, started_at DESC);

-- Subscribers to a fire's terminal Result.
CREATE TABLE fire_subscribers (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT REFERENCES agents(id)   ON DELETE CASCADE,    -- applies to every fire of the agent
  trigger_id    TEXT REFERENCES triggers(id) ON DELETE CASCADE,    -- applies only to this trigger
  kind          TEXT NOT NULL CHECK(kind IN ('inject_parent','webhook','notify_native','chain_agent')),
  config        TEXT NOT NULL,                                     -- JSON, see §4.1
  enabled       INTEGER NOT NULL DEFAULT 1,
  order_index   INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL,
  CHECK (agent_id IS NOT NULL OR trigger_id IS NOT NULL)
);
CREATE INDEX fire_subscribers_agent_idx   ON fire_subscribers(agent_id,   enabled);
CREATE INDEX fire_subscribers_trigger_idx ON fire_subscribers(trigger_id, enabled);

-- Audit / debug trail for subscriber execution.
CREATE TABLE fire_subscriber_runs (
  id                TEXT PRIMARY KEY,
  fire_id           TEXT NOT NULL REFERENCES fires(id) ON DELETE CASCADE,
  subscriber_id     TEXT NOT NULL REFERENCES fire_subscribers(id) ON DELETE CASCADE,
  status            TEXT NOT NULL CHECK(status IN ('ok','error','skipped')),
  started_at        TEXT NOT NULL,
  finished_at       TEXT,
  error_message     TEXT,
  response_snippet  TEXT                                            -- first 4 KB of webhook response body
);
CREATE INDEX fire_subscriber_runs_fire_idx ON fire_subscriber_runs(fire_id);
```

**Deliberately absent:** no `sessions` table (roy is the source of truth), no
`users` / `teams` / `projects`, no encrypted secrets, no skills/persona file
references. `task` carries the whole user-turn prompt.

### 4.1 Subscriber `config` shapes (JSON)

| `kind`          | `config`                                                                                                                                                                                                                                                                            | Behaviour                                                                                                                                                                                                                          |
|-----------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `inject_parent` | `{ "session_id": "<roy session>", "prefix": "..." }` — `prefix` optional.                                                                                                                                                                                                           | `Resume` the parent session and send the result as the next user turn (`prefix` + `assistant_text`). If parent is busy, `WaitForResult` first (5 min timeout), then send. (A future `"format": "summary"` mode is reserved; v1 rejects unknown keys at insert time so the schema stays clean.) |
| `webhook`       | `{ "url": "...", "method": "POST", "headers": {"k":"v"}, "body_template": "..." }` — `method` defaults to `POST`, `headers` optional, `body_template` rendered with the placeholder context in §4.2.                                                                                | One HTTP request. No retry in v1. Records HTTP status and first 4 KB of body in `fire_subscriber_runs.response_snippet`. Slack / Discord / Telegram / arbitrary backends all collapse onto this.                                   |
| `notify_native` | `{ "title": "...", "sound": "..." }` — both optional.                                                                                                                                                                                                                               | macOS: `osascript -e 'display notification ...'`. Linux: `notify-send`. Zero-config "ping me when done".                                                                                                                           |
| `chain_agent`   | `{ "target_agent_id": "...", "prompt_template": "..." }` — placeholder.                                                                                                                                                                                                             | **v1: rejected with `not_implemented`.** Schema reserves the slot so a future migration doesn't break enum check.                                                                                                                  |

Subscribers run **sequentially in `order_index` ASC, then `created_at` ASC**
(tiebreaker is deterministic). An error in one does not abort the others.
Each one's outcome is recorded in `fire_subscriber_runs`. **At-most-once**
delivery per fire — no automatic retry (a re-run requires a fresh fire).

### 4.2 Webhook template context

`body_template` is rendered with a minimal Handlebars-style engine
(`{{ ... }}` substitution; no helpers, no conditionals in v1 — JSON-quoting
is up to the template author). Available variables:

| Placeholder                           | Source                                                                  |
|---------------------------------------|-------------------------------------------------------------------------|
| `{{agent.id}}` / `{{agent.name}}`     | `agents` row                                                            |
| `{{trigger.id}}`                      | `triggers.id` (empty string on ad-hoc fires)                            |
| `{{fire.id}}`                         | `fires.id`                                                              |
| `{{fire.started_at}}` / `{{fire.finished_at}}` | ISO-8601 UTC                                                  |
| `{{fire.duration_ms}}`                | `finished_at - started_at`                                              |
| `{{fire.status}}`                     | `ok` / `error` / `timeout`                                              |
| `{{fire.cost_usd}}`                   | from terminal `Result`                                                  |
| `{{fire.stop_reason}}`                | from terminal `Result`                                                  |
| `{{session.id}}`                      | `fires.session_id`                                                      |
| `{{result.assistant_text}}`           | concatenated assistant turns from `(seq_start..=seq_end)`               |
| `{{result.error_message}}`            | populated only when `fire.status != 'ok'`                               |

Unknown placeholders render as empty string. The template author is
responsible for escaping (e.g. wrapping `assistant_text` in `"..."` and
JSON-encoding for Slack).

## 5. Driver loop

Structure deliberately matches `bridge/scheduler.ts` in `claude-agent` — it
is a known-good shape:

```
            ┌─────────────────────────────────────────────────────────┐
            │ scheduler driver (single tokio task)                    │
            │                                                         │
  every     │  ┌──────────┐    ┌──────────┐    ┌─────────────────┐    │
  1500 ms ─▶│  │ pollTick │──▶ │ planTick │──▶ │ apply mutations │    │
            │  │  (txn)   │    │ (pure fn)│    │   (same txn)    │    │
            │  └──────────┘    └──────────┘    └────────┬────────┘    │
            │                                           │             │
            │                                           ▼             │
            │                              ┌──────────────────────┐   │
            │                              │ semaphore.run(       │   │
            │                              │   invoke_fire(row)   │   │  ← outside the txn
            │                              │ ) × N ≤ MAX_FIRES    │   │
            │                              └──────────┬───────────┘   │
            └─────────────────────────────────────────┼───────────────┘
                                                      │
                                                      ▼
                                    ┌──────────────────────────────────┐
                                    │ invoke_fire                      │
                                    │  1. INSERT fires (status=running)│
                                    │  2. roy.Fire { tags: {agent_id,  │
                                    │       trigger_id, fire_id, ...} }│
                                    │  3. await FireDone / Timeout     │
                                    │  4. UPDATE fires (status, ...)   │
                                    │  5. if persistent: UPDATE        │
                                    │     agents.persistent_session_id │
                                    │  6. run subscribers in order:    │
                                    │     ├─ inject_parent             │
                                    │     ├─ webhook                   │
                                    │     └─ notify_native             │
                                    │  7. INSERT fire_subscriber_runs  │
                                    └──────────────────────────────────┘
```

`planTick` is a pure function with the contract:

```rust
fn plan_tick(
    rows: &[TriggerRow],
    now: DateTime<Utc>,
    compute_next: impl Fn(&str, &str /* tz */) -> Option<DateTime<Utc>>,
) -> TickPlan { /* { to_delete, to_advance, to_pause, to_fire } */ }
```

Ported from `lib/scheduler-plan.ts` in claude-agent; tests come with it.
Rules:
- `kind=oneshot` → delete + fire.
- `kind=cron`, valid expression → advance `next_fire_at`, fire.
- `kind=cron`, invalid → set `paused=true, last_error='invalid cron'` (no
  fire). This is the only way a row exits the due-set without firing, so it
  cannot hot-loop.

`compute_next` reads the per-trigger `timezone` column when computing the
next cron occurrence; that value defaults to `ROY_SCHEDULER_TZ` at trigger
creation time and is fixed for the trigger's life (changing it = remove +
re-add).

### 5.1 Tuning knobs (env vars)

| Variable                        | Default                                  |
|---------------------------------|------------------------------------------|
| `ROY_SCHEDULER_POLL_MS`         | `1500`                                   |
| `ROY_SCHEDULER_BATCH`           | `50`                                     |
| `ROY_SCHEDULER_MAX_FIRES`       | `8`                                      |
| `ROY_SCHEDULER_TZ`              | `UTC`                                    |
| `ROY_SCHEDULER_DB`              | `~/.local/state/roy-scheduler/state.db`  |
| `ROY_SOCKET`                    | (inherited from roy)                     |

### 5.2 CLI surface (`roy-scheduler`)

```
roy-scheduler serve                                                # run the driver loop

roy-scheduler agents add --name X --preset claude --cwd ... --task '...' [--persistent]
roy-scheduler agents list
roy-scheduler agents show <agent-id>
roy-scheduler agents rm   <agent-id>

roy-scheduler triggers add --agent X --cron '0 9 * * *' [--tz Europe/Moscow]
roy-scheduler triggers add --agent X --oneshot 2026-05-25T10:00:00+03:00
roy-scheduler triggers list [--agent X]
roy-scheduler triggers rm <trigger-id>
roy-scheduler triggers pause|resume <trigger-id>

roy-scheduler subscribers add (--trigger T | --agent A) --kind <K> --config '{...}'   # --trigger and --agent are mutually exclusive
roy-scheduler subscribers list [--agent X | --trigger Y]
roy-scheduler subscribers rm <subscriber-id>

roy-scheduler fires list [--agent X] [--limit 20]
roy-scheduler fires show <fire-id>                                 # streams from roy journal via fires.session_id

roy-scheduler fire-now <agent-id>                                  # ad-hoc fire, bypasses scheduling
```

Single-instance guard via `PidLock` on `~/.local/state/roy-scheduler/serve.pid`
(same primitive used by roy itself).

## 6. PG-readiness (commitments now, work later)

The scheduler ships on SQLite. Migration to Postgres later must be
**mechanical**, not a DAO rewrite. Three decisions enforce that:

1. **`sqlx`, not `rusqlite`.** Same query API, same macros, both backends via
   cargo features (`sqlite` for now, `postgres` later).
2. **Portable types only.**
   | Logical             | SQLite        | Postgres        | Rust                       |
   |---------------------|---------------|-----------------|----------------------------|
   | Timestamp           | `TEXT` (ISO-8601 UTC) | `TIMESTAMPTZ`   | `chrono::DateTime<Utc>`    |
   | Boolean             | `INTEGER` 0/1 | `BOOLEAN`       | `bool`                     |
   | JSON                | `TEXT`        | `JSONB`         | `serde_json::Value`        |
   | Primary key         | `TEXT` (ulid) | `TEXT` (ulid)   | `String`                   |
   ISO-8601 UTC strings sort lexicographically, so `next_fire_at <= ?` works
   identically in both. No `rowid` use, no `INTEGER PRIMARY KEY AUTOINCREMENT`.
3. **Parallel migration directories.** `migrations/sqlite/0001_initial.sql`
   and `migrations/postgres/0001_initial.sql`. Identical semantics, different
   syntax. Every schema change touches both files in the same commit.

### 6.1 Forbidden patterns

- No SQLite-only constructs: `WITHOUT ROWID`, FTS5, virtual tables, `json_extract`/`json_set` in queries, `INSERT OR REPLACE`/`INSERT OR IGNORE`.
- JSON is parsed in Rust (`serde_json::Value`), never queried into. Future PG
  build can add `jsonb` indices on top; SQLite code keeps working.
- Standard upsert: `INSERT … ON CONFLICT (id) DO UPDATE/NOTHING` — works on
  SQLite ≥ 3.24 and PG ≥ 9.5.

### 6.2 The only dialect-specific code

The claim transaction in `pollOnce`. SQLite needs no row locks (single
writer). PG adds `FOR UPDATE SKIP LOCKED` for multi-instance future:

```rust
#[cfg(feature = "sqlite")]
async fn select_due(tx: &mut Transaction<'_, Sqlite>, now: &str, batch: i64) -> Vec<TriggerRow> { /* … */ }

#[cfg(feature = "postgres")]
async fn select_due(tx: &mut Transaction<'_, Postgres>, now: &str, batch: i64) -> Vec<TriggerRow> {
    // … FOR UPDATE SKIP LOCKED
}
```

Everything else (`planTick`, mutations, fire loop, subscribers) is one code
path.

### 6.3 Migration runbook (deferred work, documented here)

1. Build `roy-scheduler` with `--no-default-features --features postgres`.
2. `roy-scheduler migrate` — creates the schema on the empty PG database.
3. `roy-scheduler migrate-from-sqlite --src ~/.local/state/roy-scheduler/state.db`
   — dumps tables in FK order (`agents → triggers → fires →
   fire_subscribers → fire_subscriber_runs`) and inserts into PG. Idempotent
   on primary keys.
4. Swap `DATABASE_URL`, restart.
5. Multi-instance is now possible (SKIP LOCKED in place); not exercised in v1.

## 7. Failure modes

| Situation                                                    | Behaviour                                                                                                                                                                                                                                                                            |
|--------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `roy` daemon down at tick time                               | `Fire` fails with network-error → `fires.status='error'`, `error_message='roy unreachable'`. The trigger has already advanced to the next slot. **No automatic retry** of the missed window: dropping a window is preferable to doubled load.                                        |
| `roy` daemon dies mid-fire                                   | RPC sees EOF → `fires.status='error', error_message='roy disconnected'`. Subscribers do not run.                                                                                                                                                                                     |
| `roy-scheduler` dies mid-fire                                | On restart, sweep: `UPDATE fires SET status='error', error_message='scheduler crashed' WHERE status='running' AND started_at < now() - 15 min`. Subscribers do not run for swept-up fires (at-most-once).                                                                             |
| Parent session deleted (`inject_parent`)                     | Roy returns `SessionNotFound` → `fire_subscriber_runs.status='error'`. Remaining subscribers proceed.                                                                                                                                                                                |
| Parent session busy mid-turn (`inject_parent`)               | Subscriber first issues `WaitForResult` against the parent (5 min timeout), then `Resume`. On timeout: `status='error', error_message='parent stayed busy'`.                                                                                                                         |
| Webhook returns 5xx or times out                             | `status='error'`, `response_snippet` populated. No retry in v1.                                                                                                                                                                                                                      |
| Invalid cron at CLI insert                                   | CLI rejects (`croner` parse + first-fire calculation).                                                                                                                                                                                                                               |
| Invalid cron already in DB                                   | `planTick` flags `paused=true, last_error='invalid cron'` (cannot hot-loop).                                                                                                                                                                                                         |
| Persistent fire, `persistent_session_id` points at gone roy session | Roy returns `SessionNotFound` → scheduler falls back to `Spawn { preset, cwd }` taken from the agent row, updates `persistent_session_id` to the new id, logs a warning.                                                                                                       |
| Second `roy-scheduler serve` started                         | PidLock refuses with `roy-scheduler already running (pid N)`.                                                                                                                                                                                                                        |

## 8. Testing strategy

| Layer                           | Approach                                                                                                                                                                                                              |
|---------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `planTick`                      | Pure function — unit tests with hand-built `TriggerRow`s and a deterministic `compute_next`. Ported from claude-agent's existing tests.                                                                               |
| SQLite store                    | Integration tests with a temp DB file per test; in-process sqlx pool. Each test wipes via `migrate fresh`.                                                                                                            |
| roy extensions (tags, WaitForResult, Fire) | Daemon-level tests in `crates/roy/src/daemon.rs` `#[cfg(test)]` driven through `tokio::io::duplex` (same shape as the existing tests).                                                                       |
| `roy-scheduler` driver loop     | Drives against a **mock roy daemon** (`tokio::io::duplex` scripted with canned `Fire`/`WaitForResult` replies). Validates: tag propagation, persistent-session reuse, subscriber order, at-most-once on crash sweep.   |
| Webhook subscriber              | Backed by `wiremock` or a hand-rolled `axum` test server.                                                                                                                                                              |
| Boundary regression test        | `cargo test -p roy-scheduler` builds with `roy = { ..., default-features = false, features = ["protocol"] }` and fails to compile if the scheduler reaches into removed internals.                                    |
| End-to-end smoke (`#[ignore]`)  | One test spawns real `roy serve`, real `roy-scheduler serve`, fakes the ACP agent with the existing `tests/scripts/fake-acp-agent.py`, registers an agent + cron-every-second trigger + webhook subscriber, asserts both `fires` and `fire_subscriber_runs` rows materialize. |

## 9. Open questions for the implementation plan

These are sized like sub-tasks, not unknowns blocking design:

- Cron crate choice: `croner` (recent, ISO-style) vs `cron-parser` port. Both
  feasible; pick when writing the plan.
- HTTP client for webhooks: `reqwest` (heavy but standard) vs `ureq`/`hyper`
  alone. Default to `reqwest` unless bundle size matters.
- Should `roy-scheduler agents show` stream the most recent persistent-session
  journal inline, or just print the id and let the user `roy attach`? Lean
  toward the latter for minimalism.
- Where the binary `roy-scheduler` lives: own crate (`crates/roy-scheduler`,
  lib + bin) is the proposal; alternative is a `roy-scheduler-cli` split
  mirroring `roy` / `roy-cli`. v1 sticks with single crate; split when the
  bin grows beyond a few hundred lines.

## 10. Out of scope (revisited, for clarity)

The features below are **explicitly deferred** so the v1 implementation plan
stays bounded:

- `chain_agent` subscriber and loop-detection.
- Webhook retry policy, backoff, dead-letter queue.
- Secret encryption (keychain / sops / agent).
- Multi-tenant / users / teams / projects.
- Any web UI.
- Postgres backend (PG-readiness only).
- Multi-instance scheduling.
- Persona/skill injection à la `~/.claude/agents/<slug>.md` hooks.
- Migration tools from claude-agent's Postgres schema (manual one-time port
  if needed).
