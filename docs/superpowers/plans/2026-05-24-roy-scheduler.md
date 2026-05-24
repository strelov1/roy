# Plan B — roy-scheduler from scratch

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crates/roy-scheduler` from scratch per the background-agents spec: cron/one-shot dispatcher for roy `Fire` calls, with pluggable subscribers (inject_parent / webhook / notify_native) on top of a 5-table SQLite store, talking to roy daemon only via the public control protocol.

**Architecture:** New workspace crate `roy-scheduler` (single lib+bin). Imports only `roy::{control::*, protocol-types}` — never reaches into `Daemon`/`SessionManager`/`Journal` internals. SQLite via sqlx (PG-ready), planTick is a pure function ported from `claude-agent/lib/scheduler-plan.ts`, driver does claim-in-txn → fire-outside-txn with a bounded semaphore. The MVP at `crates/roy-scheduler/` (commit `fe12d52` "feat(scheduler): roy-scheduler — cron-driven Fire dispatcher") gets deleted in Task 1 — its schema and approach diverge from the spec.

**Tech Stack:**
- Rust 2021, tokio (full)
- `sqlx 0.8` (sqlite + chrono + macros + migrate) — PG-ready
- `croner 2.x` (cron expressions with timezone)
- `chrono 0.4` (timestamps; ISO-8601 UTC strings in DB)
- `reqwest 0.12` (webhook subscriber, blocking off, json feature)
- `clap 4.5` (CLI, derive + env)
- `serde 1.0` + `serde_json 1.0`
- `anyhow`, `tracing`, `tracing-subscriber`
- `uuid 1` (ulid-style ids; we use uuid v4 strings since ulid is one more dep)
- `roy = { path = "../roy" }` for `ClientCommand`/`ServerEvent`/`FireTarget`/`TurnEvent`/`StopReason`

**Spec reference:** `/Users/i_strelov/Projects/roy/docs/superpowers/specs/2026-05-23-background-agents-design.md`. Each task references the spec section it implements.

**Boundary rule (enforce in CLAUDE.md after merge):** `roy-scheduler` may only import `roy::control`, `roy::event`, and `roy::FireTarget`. Never `roy::daemon`, `roy::manager`, `roy::engine`, `roy::journal`, `roy::transport`. Code reviewer should reject any other import path.

---

## File map

```
crates/roy-scheduler/
  Cargo.toml                                  # new (Task 2)
  migrations/
    sqlite/
      0001_initial.sql                        # 5-table schema per spec §4 (Task 3)
    postgres/
      0001_initial.sql                        # parallel PG version, never run in v1 (Task 3)
  src/
    lib.rs                                    # pub mod declarations + re-exports (Task 2)
    main.rs                                   # CLI entry (Task 2 stub → Task 17 full)
    db.rs                                     # SqlitePool + sqlx::migrate (Task 3)
    types.rs                                  # Agent/Trigger/Fire/Subscriber/SubscriberRun (Task 4)
    store/
      mod.rs                                  # pub re-exports + shared helpers (Task 5)
      agents.rs                               # agents CRUD (Task 5)
      triggers.rs                             # triggers CRUD + select_due/advance/pause (Task 6)
      fires.rs                                # fires CRUD + sweep_running (Task 7)
      subscribers.rs                          # subscribers + subscriber_runs CRUD (Task 8)
    plan.rs                                   # planTick pure fn (Task 9)
    roy_client.rs                             # protocol client over UDS (Task 10)
    subscribers/
      mod.rs                                  # SubscriberKind dispatch + Context (Task 14)
      inject_parent.rs                        # Resume + WaitForResult + Resume-send (Task 11)
      webhook.rs                              # template render + reqwest POST (Task 12)
      notify_native.rs                        # osascript / notify-send (Task 13)
    driver.rs                                 # poll_tick + invoke_fire + serve loop (Tasks 15-16)
  tests/
    e2e.rs                                    # one #[ignore]d end-to-end test (Task 18)
```

No edits to `crates/roy`, `crates/roy-cli`, or `roy-cli/src/mcp.rs` — Plan A already shipped everything roy-side that this plan needs.

---

## Pre-flight read (5 minutes before starting)

- `/Users/i_strelov/Projects/roy/docs/superpowers/specs/2026-05-23-background-agents-design.md` — full spec (§4 schema, §5 driver, §6 PG-readiness, §7 failure modes)
- `/Users/i_strelov/Projects/roy/crates/roy/src/control.rs` lines 117-220 — wire shape of `ClientCommand::Fire` / `FireTarget` / `ServerEvent::FireDone|FireTimeout|FireError`
- `/Users/i_strelov/Projects/roy/crates/roy/src/event.rs` — `TurnEvent::Result { cost_usd, stop_reason }`, `StopReason::is_error()`
- `~/Projects/claude-agent/lib/scheduler-plan.ts` — the planTick we are porting in Task 9 (TypeScript, ~50 lines, faithful translation)

---

## Task 1: Demolish the MVP scaffolding

**Files:**
- Delete contents of: `crates/roy-scheduler/src/main.rs`
- Delete contents of: `crates/roy-scheduler/Cargo.toml`
- Keep directory `crates/roy-scheduler/` (Task 2 repopulates)

The current MVP (`fe12d52`) implements a different schema (single `tasks` table) and approach (no planTick, no subscriber pluggability, no inject_parent). User decision: scrap and rewrite per spec.

- [ ] **Step 1: Inspect current state to know what we're throwing out**

```bash
git log --oneline -1 crates/roy-scheduler/
wc -l crates/roy-scheduler/src/*.rs crates/roy-scheduler/Cargo.toml
```

Expected: `fe12d52 feat(scheduler): ...`, around 420 lines total.

- [ ] **Step 2: Empty the source files**

```bash
: > crates/roy-scheduler/src/main.rs
: > crates/roy-scheduler/Cargo.toml
```

- [ ] **Step 3: Verify workspace no longer builds (expected — empty Cargo.toml)**

```bash
cargo build --workspace --all-targets 2>&1 | head -10
```

Expected: error from `roy-scheduler` Cargo.toml being empty.

- [ ] **Step 4: Remove the broken stub from the workspace temporarily**

Edit root `/Users/i_strelov/Projects/roy/Cargo.toml` from:

```toml
[workspace]
resolver = "2"
members = ["crates/*"]
```

…to:

```toml
[workspace]
resolver = "2"
members = ["crates/roy", "crates/roy-cli"]
```

(Explicit list so the empty `roy-scheduler` directory doesn't get globbed.) This is **temporary** — Task 2 restores `crates/*`.

- [ ] **Step 5: Verify workspace builds without roy-scheduler**

```bash
cargo build --workspace --all-targets 2>&1 | tail -3
cargo test --workspace --no-fail-fast 2>&1 | grep "test result"
```

Expected: 80/80 tests pass.

- [ ] **Step 6: Commit the demolition**

```bash
git add Cargo.toml crates/roy-scheduler/src/main.rs crates/roy-scheduler/Cargo.toml
git commit -m "chore(scheduler): demolish MVP, will rewrite per background-agents spec

The MVP at fe12d52 was a single-tasks-table scratch dispatcher with no
agent identity, no planTick discipline, and no subscriber pluggability —
all of which the formal spec at docs/superpowers/specs/2026-05-23-...
requires. Easier to rebuild than to evolve. Workspace temporarily
excludes the empty crate; Task 2 restores it with the new shape."
```

---

## Task 2: Cargo.toml + minimal `lib.rs`/`main.rs` so the crate compiles

**Files:**
- Write: `crates/roy-scheduler/Cargo.toml`
- Write: `crates/roy-scheduler/src/lib.rs`
- Write: `crates/roy-scheduler/src/main.rs`
- Modify: `/Users/i_strelov/Projects/roy/Cargo.toml` (restore `members = ["crates/*"]`)

- [ ] **Step 1: Write Cargo.toml**

```toml
[package]
name = "roy-scheduler"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[[bin]]
name = "roy-scheduler"
path = "src/main.rs"

[dependencies]
roy = { path = "../roy" }

# DB (sqlite default; postgres feature deferred per spec §6.1)
sqlx = { version = "0.8", default-features = false, features = [
  "runtime-tokio",
  "sqlite",
  "chrono",
  "macros",
  "migrate",
] }

# Time + cron
chrono = { version = "0.4", default-features = false, features = ["serde", "clock"] }
chrono-tz = "0.10"
croner = "2"

# HTTP (webhook subscriber)
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }

# Async runtime
tokio = { version = "1", features = ["full"] }

# CLI
clap = { version = "4.5", features = ["derive", "env"] }

# Serde / errors / logging / ids
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4", "serde"] }

[dev-dependencies]
tempfile = "3"
wiremock = "0.6"
```

- [ ] **Step 2: Write `src/lib.rs`**

```rust
//! `roy-scheduler` — cron + one-shot fire dispatcher for roy.
//!
//! Spec: docs/superpowers/specs/2026-05-23-background-agents-design.md
//!
//! Boundary rule: imports from `roy` only the control protocol
//! (`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`,
//! `StopReason`). Never reaches into Daemon, SessionManager, Engine,
//! Journal, Transport.

pub mod db;
pub mod driver;
pub mod plan;
pub mod roy_client;
pub mod store;
pub mod subscribers;
pub mod types;
```

- [ ] **Step 3: Write `src/main.rs` — minimal stub**

```rust
//! `roy-scheduler` binary — CLI entry. Filled in by later tasks.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("roy_scheduler=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    eprintln!("roy-scheduler: stub — CLI lands in Task 17");
    Ok(())
}
```

- [ ] **Step 4: Create empty module files so `lib.rs` compiles**

```bash
touch crates/roy-scheduler/src/db.rs
touch crates/roy-scheduler/src/driver.rs
touch crates/roy-scheduler/src/plan.rs
touch crates/roy-scheduler/src/roy_client.rs
touch crates/roy-scheduler/src/types.rs
mkdir -p crates/roy-scheduler/src/store
touch crates/roy-scheduler/src/store/mod.rs
mkdir -p crates/roy-scheduler/src/subscribers
touch crates/roy-scheduler/src/subscribers/mod.rs
```

- [ ] **Step 5: Restore workspace `members = ["crates/*"]`**

Edit root `/Users/i_strelov/Projects/roy/Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/*"]
```

- [ ] **Step 6: Build + verify**

```bash
cargo build --workspace --all-targets 2>&1 | tail -5
```

Expected: clean build (downloads new dep tree on first run — sqlx, croner, reqwest will take a few minutes to compile).

```bash
cargo test --workspace --no-fail-fast 2>&1 | grep "test result"
```

Expected: 80/80 (roy + roy-cli unchanged), `roy-scheduler` reports `0 passed` (no tests yet).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/roy-scheduler/
git commit -m "feat(scheduler): empty Cargo + lib/main scaffolding

New crate manifest with sqlx/croner/reqwest/clap deps; lib.rs declares
the module tree; main.rs is a stub. Subsequent tasks fill the modules.

Boundary rule documented in lib.rs: imports from roy crate are limited
to the control protocol — never internals."
```

---

## Task 3: SQLite schema + db.rs (pool + auto-migrate)

**Files:**
- Create: `crates/roy-scheduler/migrations/sqlite/0001_initial.sql`
- Create: `crates/roy-scheduler/migrations/postgres/0001_initial.sql` (parallel, never run in v1)
- Modify: `crates/roy-scheduler/src/db.rs`

Schema is **verbatim** from spec §4. Both files maintained in lock-step (spec §6.1 forbidden patterns: no SQLite-only constructs, no `WITHOUT ROWID`, no `INSERT OR REPLACE`, ISO-8601 TEXT for timestamps).

- [ ] **Step 1: Write the SQLite migration**

`crates/roy-scheduler/migrations/sqlite/0001_initial.sql`:

```sql
-- spec §4. Created with mode 0600 in src/db.rs.

CREATE TABLE agents (
  id                       TEXT PRIMARY KEY,
  name                     TEXT NOT NULL,
  preset                   TEXT NOT NULL,
  project_id               TEXT,
  task                     TEXT NOT NULL,
  model                    TEXT,
  persistent               INTEGER NOT NULL DEFAULT 0,
  persistent_session_id    TEXT,
  created_at               TEXT NOT NULL,
  updated_at               TEXT NOT NULL
);

CREATE TABLE triggers (
  id              TEXT PRIMARY KEY,
  agent_id        TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  kind            TEXT NOT NULL CHECK(kind IN ('cron','oneshot')),
  cron_expr       TEXT,
  timezone        TEXT NOT NULL DEFAULT 'UTC',
  fire_at         TEXT,
  next_fire_at    TEXT NOT NULL,
  last_fire_at    TEXT,
  paused          INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  created_at      TEXT NOT NULL
);
CREATE INDEX triggers_due_idx ON triggers(paused, next_fire_at);

CREATE TABLE fires (
  id                          TEXT PRIMARY KEY,
  agent_id                    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  trigger_id                  TEXT REFERENCES triggers(id) ON DELETE SET NULL,
  session_id                  TEXT,
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

CREATE TABLE fire_subscribers (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT REFERENCES agents(id)   ON DELETE CASCADE,
  trigger_id    TEXT REFERENCES triggers(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK(kind IN ('inject_parent','webhook','notify_native','chain_agent')),
  config        TEXT NOT NULL,
  enabled       INTEGER NOT NULL DEFAULT 1,
  order_index   INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL,
  CHECK (agent_id IS NOT NULL OR trigger_id IS NOT NULL)
);
CREATE INDEX fire_subscribers_agent_idx   ON fire_subscribers(agent_id,   enabled);
CREATE INDEX fire_subscribers_trigger_idx ON fire_subscribers(trigger_id, enabled);

CREATE TABLE fire_subscriber_runs (
  id                TEXT PRIMARY KEY,
  fire_id           TEXT NOT NULL REFERENCES fires(id) ON DELETE CASCADE,
  subscriber_id     TEXT NOT NULL REFERENCES fire_subscribers(id) ON DELETE CASCADE,
  status            TEXT NOT NULL CHECK(status IN ('ok','error','skipped')),
  started_at        TEXT NOT NULL,
  finished_at       TEXT,
  error_message     TEXT,
  response_snippet  TEXT
);
CREATE INDEX fire_subscriber_runs_fire_idx ON fire_subscriber_runs(fire_id);
```

- [ ] **Step 2: Write the parallel PG migration**

`crates/roy-scheduler/migrations/postgres/0001_initial.sql`:

```sql
-- spec §4 in Postgres dialect. Mirrored from migrations/sqlite/0001_initial.sql;
-- maintained in lock-step per spec §6.1. Not run in v1.

CREATE TABLE agents (
  id                       TEXT PRIMARY KEY,
  name                     TEXT NOT NULL,
  preset                   TEXT NOT NULL,
  project_id               TEXT,
  task                     TEXT NOT NULL,
  model                    TEXT,
  persistent               BOOLEAN NOT NULL DEFAULT FALSE,
  persistent_session_id    TEXT,
  created_at               TIMESTAMPTZ NOT NULL,
  updated_at               TIMESTAMPTZ NOT NULL
);

CREATE TABLE triggers (
  id              TEXT PRIMARY KEY,
  agent_id        TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  kind            TEXT NOT NULL CHECK(kind IN ('cron','oneshot')),
  cron_expr       TEXT,
  timezone        TEXT NOT NULL DEFAULT 'UTC',
  fire_at         TIMESTAMPTZ,
  next_fire_at    TIMESTAMPTZ NOT NULL,
  last_fire_at    TIMESTAMPTZ,
  paused          BOOLEAN NOT NULL DEFAULT FALSE,
  last_error      TEXT,
  created_at      TIMESTAMPTZ NOT NULL
);
CREATE INDEX triggers_due_idx ON triggers(paused, next_fire_at);

CREATE TABLE fires (
  id                          TEXT PRIMARY KEY,
  agent_id                    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  trigger_id                  TEXT REFERENCES triggers(id) ON DELETE SET NULL,
  session_id                  TEXT,
  status                      TEXT NOT NULL CHECK(status IN ('running','ok','error','timeout')),
  started_at                  TIMESTAMPTZ NOT NULL,
  finished_at                 TIMESTAMPTZ,
  transcript_seq_range_start  BIGINT,
  transcript_seq_range_end    BIGINT,
  assistant_text              TEXT,
  cost_usd                    DOUBLE PRECISION,
  stop_reason                 TEXT,
  error_message               TEXT
);
CREATE INDEX fires_agent_idx ON fires(agent_id, started_at DESC);

CREATE TABLE fire_subscribers (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT REFERENCES agents(id)   ON DELETE CASCADE,
  trigger_id    TEXT REFERENCES triggers(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK(kind IN ('inject_parent','webhook','notify_native','chain_agent')),
  config        JSONB NOT NULL,
  enabled       BOOLEAN NOT NULL DEFAULT TRUE,
  order_index   INTEGER NOT NULL DEFAULT 0,
  created_at    TIMESTAMPTZ NOT NULL,
  CHECK (agent_id IS NOT NULL OR trigger_id IS NOT NULL)
);
CREATE INDEX fire_subscribers_agent_idx   ON fire_subscribers(agent_id,   enabled);
CREATE INDEX fire_subscribers_trigger_idx ON fire_subscribers(trigger_id, enabled);

CREATE TABLE fire_subscriber_runs (
  id                TEXT PRIMARY KEY,
  fire_id           TEXT NOT NULL REFERENCES fires(id) ON DELETE CASCADE,
  subscriber_id     TEXT NOT NULL REFERENCES fire_subscribers(id) ON DELETE CASCADE,
  status            TEXT NOT NULL CHECK(status IN ('ok','error','skipped')),
  started_at        TIMESTAMPTZ NOT NULL,
  finished_at       TIMESTAMPTZ,
  error_message     TEXT,
  response_snippet  TEXT
);
CREATE INDEX fire_subscriber_runs_fire_idx ON fire_subscriber_runs(fire_id);
```

- [ ] **Step 3: Write `src/db.rs` — pool + migrate**

```rust
//! SQLite pool + auto-migrate. The pool is configured with WAL mode and
//! a 5-second busy timeout per spec §4. File is created with mode 0600
//! since `config` columns hold plain JSON that may contain webhook
//! tokens.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

/// Run the bundled SQLite migrations against this pool.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("migrations/sqlite");

/// Open or create the SQLite database at `path`, apply migrations, and
/// return a connection pool. Sets mode 0600 on Unix.
pub async fn open(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("opening SQLite at {}", path.display()))?;

    MIGRATOR.run(&pool).await.context("running migrations")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }
    }

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn open_creates_db_and_applies_migrations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.db");
        let pool = open(&path).await.unwrap();

        // Verify every expected table exists.
        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let names: Vec<&str> = tables.iter().map(|(n,)| n.as_str()).collect();

        assert!(names.contains(&"agents"));
        assert!(names.contains(&"triggers"));
        assert!(names.contains(&"fires"));
        assert!(names.contains(&"fire_subscribers"));
        assert!(names.contains(&"fire_subscriber_runs"));
    }

    #[tokio::test]
    async fn open_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.db");
        let _pool = open(&path).await.unwrap();
        // Re-open the same file.
        let _pool2 = open(&path).await.unwrap();
    }
}
```

- [ ] **Step 4: Run, verify**

```bash
cargo test -p roy-scheduler db:: -- --nocapture
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-scheduler/migrations/ crates/roy-scheduler/src/db.rs
git commit -m "feat(scheduler): SQLite schema + db.rs pool/migrate

5 tables per spec §4: agents, triggers, fires, fire_subscribers,
fire_subscriber_runs. Parallel migrations/postgres/0001_initial.sql kept
in lock-step per spec §6.1 (never run in v1). WAL mode, 5s busy_timeout,
0600 perms on the file."
```

---

## Task 4: Domain types — `src/types.rs`

**Files:**
- Modify: `crates/roy-scheduler/src/types.rs`

All domain structs serde-derived. Database rows use sqlx's `FromRow` derive when feasible (chrono integration handles TIMESTAMP TEXT). JSON `config` and `tags` parsed at the edge into `serde_json::Value`.

- [ ] **Step 1: Write the types**

```rust
//! Domain types for `roy-scheduler`. Mirror the SQLite schema in
//! migrations/sqlite/0001_initial.sql; field names use snake_case to
//! match the DB columns directly via sqlx `FromRow`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub preset: String,
    pub project_id: Option<String>,
    pub task: String,
    pub model: Option<String>,
    /// SQLite INTEGER 0/1. Use the bool getter `is_persistent()` for clarity.
    pub persistent: i64,
    pub persistent_session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Agent {
    pub fn is_persistent(&self) -> bool {
        self.persistent != 0
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Trigger {
    pub id: String,
    pub agent_id: String,
    pub kind: String, // 'cron' | 'oneshot'
    pub cron_expr: Option<String>,
    pub timezone: String,
    pub fire_at: Option<DateTime<Utc>>,
    pub next_fire_at: DateTime<Utc>,
    pub last_fire_at: Option<DateTime<Utc>>,
    pub paused: i64,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Trigger {
    pub fn is_paused(&self) -> bool {
        self.paused != 0
    }

    pub fn is_oneshot(&self) -> bool {
        self.kind == "oneshot"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FireStatus {
    Running,
    Ok,
    Error,
    Timeout,
}

impl FireStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            FireStatus::Running => "running",
            FireStatus::Ok => "ok",
            FireStatus::Error => "error",
            FireStatus::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Fire {
    pub id: String,
    pub agent_id: String,
    pub trigger_id: Option<String>,
    pub session_id: Option<String>,
    pub status: String, // see FireStatus
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub transcript_seq_range_start: Option<i64>,
    pub transcript_seq_range_end: Option<i64>,
    pub assistant_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriberKind {
    InjectParent,
    Webhook,
    NotifyNative,
    /// v1 reserved; rejected with `not_implemented` if invoked.
    ChainAgent,
}

impl SubscriberKind {
    pub fn as_db(self) -> &'static str {
        match self {
            SubscriberKind::InjectParent => "inject_parent",
            SubscriberKind::Webhook => "webhook",
            SubscriberKind::NotifyNative => "notify_native",
            SubscriberKind::ChainAgent => "chain_agent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inject_parent" => Some(Self::InjectParent),
            "webhook" => Some(Self::Webhook),
            "notify_native" => Some(Self::NotifyNative),
            "chain_agent" => Some(Self::ChainAgent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Subscriber {
    pub id: String,
    pub agent_id: Option<String>,
    pub trigger_id: Option<String>,
    pub kind: String,
    /// Raw JSON string from DB. Parsers per kind live in src/subscribers/*.rs.
    pub config: String,
    pub enabled: i64,
    pub order_index: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SubscriberRun {
    pub id: String,
    pub fire_id: String,
    pub subscriber_id: String,
    pub status: String, // 'ok' | 'error' | 'skipped'
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriber_kind_roundtrips() {
        for kind in [
            SubscriberKind::InjectParent,
            SubscriberKind::Webhook,
            SubscriberKind::NotifyNative,
            SubscriberKind::ChainAgent,
        ] {
            assert_eq!(SubscriberKind::parse(kind.as_db()), Some(kind));
        }
        assert_eq!(SubscriberKind::parse("nope"), None);
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler types::
```

Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/types.rs
git commit -m "feat(scheduler): domain types matching the SQLite schema

Agent / Trigger / Fire / Subscriber / SubscriberRun with FromRow derives
so sqlx can populate them directly. SubscriberKind enum knows the four
DB strings (inject_parent / webhook / notify_native / chain_agent) and
roundtrips through parse/as_db."
```

---

## Task 5: Store — agents CRUD

**Files:**
- Create: `crates/roy-scheduler/src/store/mod.rs`
- Create: `crates/roy-scheduler/src/store/agents.rs`

- [ ] **Step 1: Write `store/mod.rs`**

```rust
//! Store layer — CRUD per table. Split per table for keep-it-small.
//!
//! All functions take `&SqlitePool` (or `&mut Transaction<'_, Sqlite>`
//! when they must run inside a claim transaction). Timestamps are
//! `DateTime<Utc>` — sqlx serializes them as ISO-8601 TEXT.

pub mod agents;
// More modules added by subsequent tasks: triggers, fires, subscribers.
```

- [ ] **Step 2: Write `store/agents.rs` with TDD test first**

```rust
//! agents table CRUD.

use anyhow::Result;
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::types::Agent;

pub struct NewAgent {
    pub name: String,
    pub preset: String,
    pub project_id: Option<String>,
    pub task: String,
    pub model: Option<String>,
    pub persistent: bool,
}

pub async fn insert(pool: &SqlitePool, new: NewAgent) -> Result<Agent> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let persistent_int: i64 = if new.persistent { 1 } else { 0 };

    sqlx::query(
        "INSERT INTO agents (id, name, preset, project_id, task, model, persistent, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.name)
    .bind(&new.preset)
    .bind(&new.project_id)
    .bind(&new.task)
    .bind(&new.model)
    .bind(persistent_int)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    get_by_id(pool, &id).await?.ok_or_else(|| anyhow::anyhow!("agent missing after insert"))
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Agent>> {
    let agent = sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(agent)
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<Agent>> {
    let agents = sqlx::query_as::<_, Agent>("SELECT * FROM agents ORDER BY created_at DESC")
        .fetch_all(pool)
        .await?;
    Ok(agents)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub async fn update_persistent_session_id(
    pool: &SqlitePool,
    agent_id: &str,
    session_id: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE agents SET persistent_session_id = ?, updated_at = ? WHERE id = ?",
    )
    .bind(session_id)
    .bind(Utc::now())
    .bind(agent_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::tempdir;

    async fn fresh_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        (dir, pool)
    }

    fn sample() -> NewAgent {
        NewAgent {
            name: "daily-digest".into(),
            preset: "claude".into(),
            project_id: None,
            task: "summarize today".into(),
            model: None,
            persistent: false,
        }
    }

    #[tokio::test]
    async fn insert_then_get_returns_same_agent() {
        let (_d, pool) = fresh_pool().await;
        let inserted = insert(&pool, sample()).await.unwrap();
        let fetched = get_by_id(&pool, &inserted.id).await.unwrap().unwrap();
        assert_eq!(inserted.id, fetched.id);
        assert_eq!(fetched.name, "daily-digest");
        assert_eq!(fetched.preset, "claude");
        assert!(!fetched.is_persistent());
    }

    #[tokio::test]
    async fn list_orders_newest_first() {
        let (_d, pool) = fresh_pool().await;
        let a1 = insert(&pool, sample()).await.unwrap();
        // ensure clock advances at least one tick
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut s2 = sample();
        s2.name = "second".into();
        let a2 = insert(&pool, s2).await.unwrap();

        let listed = list(&pool).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, a2.id, "newest first");
        assert_eq!(listed[1].id, a1.id);
    }

    #[tokio::test]
    async fn delete_removes_then_get_returns_none() {
        let (_d, pool) = fresh_pool().await;
        let a = insert(&pool, sample()).await.unwrap();
        assert!(delete(&pool, &a.id).await.unwrap());
        assert!(get_by_id(&pool, &a.id).await.unwrap().is_none());
        // second delete returns false (no row).
        assert!(!delete(&pool, &a.id).await.unwrap());
    }

    #[tokio::test]
    async fn update_persistent_session_id_round_trips() {
        let (_d, pool) = fresh_pool().await;
        let a = insert(&pool, sample()).await.unwrap();
        update_persistent_session_id(&pool, &a.id, Some("roy-sid-1"))
            .await
            .unwrap();
        let back = get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.persistent_session_id.as_deref(), Some("roy-sid-1"));

        update_persistent_session_id(&pool, &a.id, None).await.unwrap();
        let back = get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.persistent_session_id, None);
    }
}
```

- [ ] **Step 3: Run + verify**

```bash
cargo test -p roy-scheduler store::agents::
```

Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-scheduler/src/store/
git commit -m "feat(scheduler): store::agents CRUD

insert / get_by_id / list / delete / update_persistent_session_id. Four
unit tests against a fresh temp SQLite pool prove round-trip, ordering,
delete idempotency, and persistent-session-id mutation."
```

---

## Task 6: Store — triggers CRUD

**Files:**
- Modify: `crates/roy-scheduler/src/store/mod.rs` (add `pub mod triggers;`)
- Create: `crates/roy-scheduler/src/store/triggers.rs`

Functions needed: `insert`, `get_by_id`, `list_for_agent`, `select_due` (used in claim txn — takes `&mut Transaction`), `advance_next_fire`, `pause`, `unpause`, `delete`.

- [ ] **Step 1: Add module declaration**

In `crates/roy-scheduler/src/store/mod.rs`:

```rust
pub mod agents;
pub mod triggers;
```

- [ ] **Step 2: Write `store/triggers.rs`**

```rust
//! triggers table CRUD. `select_due` and `advance_next_fire` are the
//! load-bearing claim-transaction operations used by the driver.

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::types::Trigger;

pub struct NewCronTrigger {
    pub agent_id: String,
    pub cron_expr: String,
    pub timezone: String,
    pub next_fire_at: DateTime<Utc>,
}

pub struct NewOneshotTrigger {
    pub agent_id: String,
    pub fire_at: DateTime<Utc>,
}

pub async fn insert_cron(pool: &SqlitePool, new: NewCronTrigger) -> Result<Trigger> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO triggers (id, agent_id, kind, cron_expr, timezone, next_fire_at, created_at)
         VALUES (?, ?, 'cron', ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(&new.cron_expr)
    .bind(&new.timezone)
    .bind(new.next_fire_at)
    .bind(now)
    .execute(pool)
    .await?;
    get_by_id(pool, &id).await?.ok_or_else(|| anyhow::anyhow!("trigger missing after insert"))
}

pub async fn insert_oneshot(pool: &SqlitePool, new: NewOneshotTrigger) -> Result<Trigger> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO triggers (id, agent_id, kind, fire_at, next_fire_at, created_at)
         VALUES (?, ?, 'oneshot', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(new.fire_at)
    .bind(new.fire_at) // next_fire_at == fire_at for oneshot
    .bind(now)
    .execute(pool)
    .await?;
    get_by_id(pool, &id).await?.ok_or_else(|| anyhow::anyhow!("trigger missing after insert"))
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Trigger>> {
    let t = sqlx::query_as::<_, Trigger>("SELECT * FROM triggers WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(t)
}

pub async fn list_for_agent(pool: &SqlitePool, agent_id: &str) -> Result<Vec<Trigger>> {
    let v = sqlx::query_as::<_, Trigger>(
        "SELECT * FROM triggers WHERE agent_id = ? ORDER BY created_at DESC",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

/// Claim-transaction read. Returns triggers with `paused = 0` and
/// `next_fire_at <= now`, ordered oldest-due first, capped at `limit`.
/// SQLite has no SKIP LOCKED — single-writer scheduler doesn't need it.
pub async fn select_due(
    tx: &mut Transaction<'_, Sqlite>,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Trigger>> {
    let rows = sqlx::query_as::<_, Trigger>(
        "SELECT * FROM triggers
         WHERE paused = 0 AND next_fire_at <= ?
         ORDER BY next_fire_at ASC
         LIMIT ?",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows)
}

pub async fn advance_next_fire(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    next_fire_at: DateTime<Utc>,
    last_fire_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        "UPDATE triggers SET next_fire_at = ?, last_fire_at = ?, last_error = NULL
         WHERE id = ?",
    )
    .bind(next_fire_at)
    .bind(last_fire_at)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn pause(tx: &mut Transaction<'_, Sqlite>, id: &str, error: &str) -> Result<()> {
    sqlx::query("UPDATE triggers SET paused = 1, last_error = ? WHERE id = ?")
        .bind(error)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn unpause(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("UPDATE triggers SET paused = 0, last_error = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn pause_outside_txn(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("UPDATE triggers SET paused = 1 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete(tx_or_pool: &SqlitePool, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM triggers WHERE id = ?")
        .bind(id)
        .execute(tx_or_pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub async fn delete_in_txn(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM triggers WHERE id = ?")
        .bind(id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, store::agents};
    use chrono::Duration;
    use tempfile::tempdir;

    async fn fixture() -> (tempfile::TempDir, SqlitePool, String) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "do".into(),
                model: None,
                persistent: false,
            },
        )
        .await
        .unwrap();
        (dir, pool, a.id)
    }

    #[tokio::test]
    async fn select_due_returns_only_past_unpaused_rows() {
        let (_d, pool, agent_id) = fixture().await;
        let now = Utc::now();

        // Two due (one paused), one in future.
        let _due = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id: agent_id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();

        let paused_row = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id: agent_id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();
        pause_outside_txn(&pool, &paused_row.id).await.unwrap();

        let _future = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id,
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now + Duration::seconds(60),
            },
        )
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let due = select_due(&mut tx, now, 50).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(due.len(), 1, "only one unpaused-past row should be due");
    }

    #[tokio::test]
    async fn advance_then_no_longer_due() {
        let (_d, pool, agent_id) = fixture().await;
        let now = Utc::now();
        let t = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id,
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        advance_next_fire(&mut tx, &t.id, now + Duration::minutes(5), now)
            .await
            .unwrap();
        let still_due = select_due(&mut tx, now, 50).await.unwrap();
        tx.commit().await.unwrap();
        assert!(still_due.is_empty());
    }

    #[tokio::test]
    async fn pause_records_error_and_excludes_from_due() {
        let (_d, pool, agent_id) = fixture().await;
        let now = Utc::now();
        let t = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id,
                cron_expr: "garbage".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        pause(&mut tx, &t.id, "invalid cron").await.unwrap();
        tx.commit().await.unwrap();

        let back = get_by_id(&pool, &t.id).await.unwrap().unwrap();
        assert!(back.is_paused());
        assert_eq!(back.last_error.as_deref(), Some("invalid cron"));
    }

    #[tokio::test]
    async fn oneshot_next_fire_at_equals_fire_at() {
        let (_d, pool, agent_id) = fixture().await;
        let t = insert_oneshot(
            &pool,
            NewOneshotTrigger {
                agent_id,
                fire_at: Utc::now() + Duration::seconds(60),
            },
        )
        .await
        .unwrap();
        assert_eq!(t.fire_at, Some(t.next_fire_at));
        assert_eq!(t.kind, "oneshot");
    }

    #[tokio::test]
    async fn cascade_delete_when_agent_dropped() {
        let (_d, pool, agent_id) = fixture().await;
        insert_cron(
            &pool,
            NewCronTrigger {
                agent_id: agent_id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now(),
            },
        )
        .await
        .unwrap();

        // sqlite requires PRAGMA foreign_keys=ON per-connection. Verify it's on
        // (sqlx::sqlite enables it by default in 0.8).
        let _ = sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await;

        agents::delete(&pool, &agent_id).await.unwrap();
        let trigs = list_for_agent(&pool, &agent_id).await.unwrap();
        assert!(trigs.is_empty(), "FK cascade should drop child triggers");
    }
}
```

- [ ] **Step 3: Verify**

```bash
cargo test -p roy-scheduler store::triggers::
```

Expected: 5 passed. If `cascade_delete_when_agent_dropped` fails, sqlx's default `foreign_keys=ON` pragma isn't applied — investigate and either add it to `db::open` connection setup or call it per connection.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-scheduler/src/store/
git commit -m "feat(scheduler): store::triggers CRUD + claim helpers

insert_cron / insert_oneshot / select_due (txn) / advance_next_fire
(txn) / pause (txn) / pause_outside_txn / unpause / delete /
delete_in_txn / list_for_agent / get_by_id. Five unit tests cover the
claim-transaction shape: due-filtering, advance-then-no-longer-due,
pause-records-error, oneshot-next_fire_at, FK-cascade-on-agent-delete."
```

---

## Task 7: Store — fires CRUD + crash-recovery sweep

**Files:**
- Modify: `crates/roy-scheduler/src/store/mod.rs` (`pub mod fires;`)
- Create: `crates/roy-scheduler/src/store/fires.rs`

- [ ] **Step 1: Write `store/fires.rs`**

```rust
//! fires table CRUD + crash-recovery sweep used on startup.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::types::{Fire, FireStatus};

pub struct NewFire {
    pub agent_id: String,
    pub trigger_id: Option<String>,
}

/// Insert a `running` fire row. Returns the new id.
pub async fn insert_running(pool: &SqlitePool, new: NewFire) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO fires (id, agent_id, trigger_id, status, started_at)
         VALUES (?, ?, ?, 'running', ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(&new.trigger_id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(id)
}

pub struct TerminalUpdate {
    pub status: FireStatus,
    pub session_id: Option<String>,
    pub seq_range: Option<(i64, i64)>,
    pub assistant_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
}

pub async fn update_terminal(pool: &SqlitePool, id: &str, t: TerminalUpdate) -> Result<()> {
    let (seq_start, seq_end) = match t.seq_range {
        Some((s, e)) => (Some(s), Some(e)),
        None => (None, None),
    };
    sqlx::query(
        "UPDATE fires SET
            status = ?,
            session_id = COALESCE(?, session_id),
            transcript_seq_range_start = ?,
            transcript_seq_range_end = ?,
            assistant_text = ?,
            cost_usd = ?,
            stop_reason = ?,
            error_message = ?,
            finished_at = ?
         WHERE id = ?",
    )
    .bind(t.status.as_db())
    .bind(&t.session_id)
    .bind(seq_start)
    .bind(seq_end)
    .bind(&t.assistant_text)
    .bind(t.cost_usd)
    .bind(&t.stop_reason)
    .bind(&t.error_message)
    .bind(Utc::now())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sweep stuck `running` fires that started more than `older_than` ago.
/// Used on driver startup to mark crashed fires as errors.
/// Returns count of rows touched.
pub async fn sweep_running_older_than(
    pool: &SqlitePool,
    cutoff: DateTime<Utc>,
) -> Result<u64> {
    let n = sqlx::query(
        "UPDATE fires SET status = 'error',
                          error_message = 'scheduler crashed',
                          finished_at = ?
         WHERE status = 'running' AND started_at < ?",
    )
    .bind(Utc::now())
    .bind(cutoff)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n)
}

pub fn default_sweep_cutoff() -> DateTime<Utc> {
    Utc::now() - Duration::minutes(15)
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Fire>> {
    let f = sqlx::query_as::<_, Fire>("SELECT * FROM fires WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(f)
}

pub async fn list_for_agent(pool: &SqlitePool, agent_id: &str, limit: i64) -> Result<Vec<Fire>> {
    let v = sqlx::query_as::<_, Fire>(
        "SELECT * FROM fires WHERE agent_id = ? ORDER BY started_at DESC LIMIT ?",
    )
    .bind(agent_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, store::agents};
    use tempfile::tempdir;

    async fn fixture() -> (tempfile::TempDir, SqlitePool, String) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "do".into(),
                model: None,
                persistent: false,
            },
        )
        .await
        .unwrap();
        (dir, pool, a.id)
    }

    #[tokio::test]
    async fn insert_running_then_terminal_updates_status() {
        let (_d, pool, agent_id) = fixture().await;
        let fire_id = insert_running(
            &pool,
            NewFire { agent_id, trigger_id: None },
        )
        .await
        .unwrap();

        update_terminal(
            &pool,
            &fire_id,
            TerminalUpdate {
                status: FireStatus::Ok,
                session_id: Some("roy-sid".into()),
                seq_range: Some((5, 12)),
                assistant_text: Some("hello".into()),
                cost_usd: Some(0.001),
                stop_reason: Some("end_turn".into()),
                error_message: None,
            },
        )
        .await
        .unwrap();

        let f = get_by_id(&pool, &fire_id).await.unwrap().unwrap();
        assert_eq!(f.status, "ok");
        assert_eq!(f.session_id.as_deref(), Some("roy-sid"));
        assert_eq!(f.transcript_seq_range_start, Some(5));
        assert_eq!(f.transcript_seq_range_end, Some(12));
        assert_eq!(f.assistant_text.as_deref(), Some("hello"));
        assert!(f.finished_at.is_some());
    }

    #[tokio::test]
    async fn sweep_marks_old_running_as_error() {
        let (_d, pool, agent_id) = fixture().await;
        let fire_id = insert_running(
            &pool,
            NewFire { agent_id: agent_id.clone(), trigger_id: None },
        )
        .await
        .unwrap();

        // Force started_at into the past so the sweep claims it.
        let past = Utc::now() - chrono::Duration::hours(1);
        sqlx::query("UPDATE fires SET started_at = ? WHERE id = ?")
            .bind(past)
            .bind(&fire_id)
            .execute(&pool)
            .await
            .unwrap();

        let n = sweep_running_older_than(&pool, default_sweep_cutoff())
            .await
            .unwrap();
        assert_eq!(n, 1);

        let f = get_by_id(&pool, &fire_id).await.unwrap().unwrap();
        assert_eq!(f.status, "error");
        assert_eq!(f.error_message.as_deref(), Some("scheduler crashed"));

        // A fresh running fire should NOT be swept.
        let f2 = insert_running(&pool, NewFire { agent_id, trigger_id: None })
            .await
            .unwrap();
        let n2 = sweep_running_older_than(&pool, default_sweep_cutoff())
            .await
            .unwrap();
        assert_eq!(n2, 0);
        let still_running = get_by_id(&pool, &f2).await.unwrap().unwrap();
        assert_eq!(still_running.status, "running");
    }

    #[tokio::test]
    async fn list_for_agent_newest_first_with_limit() {
        let (_d, pool, agent_id) = fixture().await;
        for _ in 0..5 {
            insert_running(
                &pool,
                NewFire { agent_id: agent_id.clone(), trigger_id: None },
            )
            .await
            .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let v = list_for_agent(&pool, &agent_id, 3).await.unwrap();
        assert_eq!(v.len(), 3);
        for w in v.windows(2) {
            assert!(w[0].started_at >= w[1].started_at);
        }
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler store::fires::
```

Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/store/
git commit -m "feat(scheduler): store::fires CRUD + crash-recovery sweep

insert_running / update_terminal / sweep_running_older_than (called on
startup per spec §7 — fires running > 15 min become errors with
'scheduler crashed') / get_by_id / list_for_agent. Three tests prove
round-trip, sweep selectivity, and ordering+limit."
```

---

## Task 8: Store — subscribers and subscriber_runs CRUD

**Files:**
- Modify: `crates/roy-scheduler/src/store/mod.rs` (`pub mod subscribers;`)
- Create: `crates/roy-scheduler/src/store/subscribers.rs`

Key function: `load_for_fire(pool, agent_id, trigger_id)` returns all enabled subscribers that match `agent_id` OR `trigger_id`, sorted by `order_index ASC, created_at ASC` (deterministic tiebreaker per spec §4.1).

- [ ] **Step 1: Write `store/subscribers.rs`**

```rust
//! fire_subscribers + fire_subscriber_runs CRUD.

use anyhow::Result;
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::types::{Subscriber, SubscriberKind, SubscriberRun};

pub struct NewSubscriber {
    /// Exactly one of agent_id / trigger_id is Some.
    pub agent_id: Option<String>,
    pub trigger_id: Option<String>,
    pub kind: SubscriberKind,
    /// JSON string. Per-kind shape lives in src/subscribers/*.rs.
    pub config_json: String,
    pub order_index: i64,
}

pub async fn insert(pool: &SqlitePool, new: NewSubscriber) -> Result<Subscriber> {
    if new.agent_id.is_none() && new.trigger_id.is_none() {
        anyhow::bail!("subscriber must reference either agent_id or trigger_id");
    }
    if new.agent_id.is_some() && new.trigger_id.is_some() {
        anyhow::bail!("subscriber may not reference both agent_id and trigger_id");
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO fire_subscribers
         (id, agent_id, trigger_id, kind, config, enabled, order_index, created_at)
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(&new.trigger_id)
    .bind(new.kind.as_db())
    .bind(&new.config_json)
    .bind(new.order_index)
    .bind(now)
    .execute(pool)
    .await?;
    get_by_id(pool, &id).await?.ok_or_else(|| anyhow::anyhow!("subscriber missing after insert"))
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Subscriber>> {
    let s = sqlx::query_as::<_, Subscriber>("SELECT * FROM fire_subscribers WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(s)
}

pub async fn list_for_agent(pool: &SqlitePool, agent_id: &str) -> Result<Vec<Subscriber>> {
    let v = sqlx::query_as::<_, Subscriber>(
        "SELECT * FROM fire_subscribers WHERE agent_id = ? ORDER BY order_index, created_at",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

pub async fn list_for_trigger(pool: &SqlitePool, trigger_id: &str) -> Result<Vec<Subscriber>> {
    let v = sqlx::query_as::<_, Subscriber>(
        "SELECT * FROM fire_subscribers WHERE trigger_id = ? ORDER BY order_index, created_at",
    )
    .bind(trigger_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

/// Load all enabled subscribers that match either `agent_id` or
/// `trigger_id`. Sorted by `order_index ASC, created_at ASC` for a
/// deterministic execution order (spec §4.1).
pub async fn load_for_fire(
    pool: &SqlitePool,
    agent_id: &str,
    trigger_id: Option<&str>,
) -> Result<Vec<Subscriber>> {
    let v = sqlx::query_as::<_, Subscriber>(
        "SELECT * FROM fire_subscribers
         WHERE enabled = 1
           AND (agent_id = ? OR trigger_id = ?)
         ORDER BY order_index ASC, created_at ASC",
    )
    .bind(agent_id)
    .bind(trigger_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM fire_subscribers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub struct NewSubscriberRun {
    pub fire_id: String,
    pub subscriber_id: String,
    pub status: &'static str, // "ok" | "error" | "skipped"
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

pub async fn insert_run(pool: &SqlitePool, run: NewSubscriberRun) -> Result<()> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO fire_subscriber_runs
         (id, fire_id, subscriber_id, status, started_at, finished_at, error_message, response_snippet)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&run.fire_id)
    .bind(&run.subscriber_id)
    .bind(run.status)
    .bind(now)
    .bind(now)
    .bind(&run.error_message)
    .bind(&run.response_snippet)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_runs_for_fire(pool: &SqlitePool, fire_id: &str) -> Result<Vec<SubscriberRun>> {
    let v = sqlx::query_as::<_, SubscriberRun>(
        "SELECT * FROM fire_subscriber_runs WHERE fire_id = ? ORDER BY started_at",
    )
    .bind(fire_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, store::{agents, fires, triggers}};
    use chrono::Duration;
    use tempfile::tempdir;

    async fn fixture() -> (tempfile::TempDir, SqlitePool, String, String) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let agent = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(), preset: "claude".into(), project_id: None,
                task: "do".into(), model: None, persistent: false,
            },
        )
        .await
        .unwrap();
        let trig = triggers::insert_cron(
            &pool,
            triggers::NewCronTrigger {
                agent_id: agent.id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() + Duration::seconds(60),
            },
        )
        .await
        .unwrap();
        (dir, pool, agent.id, trig.id)
    }

    #[tokio::test]
    async fn insert_rejects_neither_or_both() {
        let (_d, pool, _a, _t) = fixture().await;
        let r = insert(
            &pool,
            NewSubscriber {
                agent_id: None, trigger_id: None,
                kind: SubscriberKind::Webhook,
                config_json: "{}".into(),
                order_index: 0,
            },
        )
        .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn load_for_fire_unions_agent_and_trigger() {
        let (_d, pool, agent_id, trig_id) = fixture().await;

        let sa = insert(
            &pool,
            NewSubscriber {
                agent_id: Some(agent_id.clone()), trigger_id: None,
                kind: SubscriberKind::Webhook,
                config_json: r#"{"url":"https://example.com"}"#.into(),
                order_index: 1,
            },
        )
        .await
        .unwrap();
        let st = insert(
            &pool,
            NewSubscriber {
                agent_id: None, trigger_id: Some(trig_id.clone()),
                kind: SubscriberKind::NotifyNative,
                config_json: "{}".into(),
                order_index: 0,
            },
        )
        .await
        .unwrap();

        let v = load_for_fire(&pool, &agent_id, Some(&trig_id)).await.unwrap();
        assert_eq!(v.len(), 2);
        // order_index 0 first
        assert_eq!(v[0].id, st.id);
        assert_eq!(v[1].id, sa.id);
    }

    #[tokio::test]
    async fn insert_run_then_list_returns_it() {
        let (_d, pool, agent_id, _t) = fixture().await;
        let fire_id = fires::insert_running(
            &pool,
            fires::NewFire { agent_id: agent_id.clone(), trigger_id: None },
        )
        .await
        .unwrap();
        let sub = insert(
            &pool,
            NewSubscriber {
                agent_id: Some(agent_id), trigger_id: None,
                kind: SubscriberKind::NotifyNative,
                config_json: "{}".into(),
                order_index: 0,
            },
        )
        .await
        .unwrap();

        insert_run(
            &pool,
            NewSubscriberRun {
                fire_id: fire_id.clone(),
                subscriber_id: sub.id.clone(),
                status: "ok",
                error_message: None,
                response_snippet: None,
            },
        )
        .await
        .unwrap();

        let v = list_runs_for_fire(&pool, &fire_id).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].status, "ok");
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler store::subscribers::
```

Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/store/
git commit -m "feat(scheduler): store::subscribers + subscriber_runs CRUD

insert (rejects neither/both agent_id+trigger_id), get_by_id,
list_for_agent/list_for_trigger, load_for_fire (unions both filters,
order_index ASC, created_at ASC for deterministic execution), delete,
insert_run, list_runs_for_fire. Three tests cover XOR enforcement,
union+ordering, and run logging."
```

---

## Task 9: planTick — pure scheduling function

**Files:**
- Modify: `crates/roy-scheduler/src/plan.rs`

Port the rules from `~/Projects/claude-agent/lib/scheduler-plan.ts` faithfully. Pure function, no I/O, no clock — caller supplies `now` and a `compute_next` closure.

- [ ] **Step 1: Write the test first (TDD)**

```rust
//! Pure per-tick decision. Mirrors claude-agent/lib/scheduler-plan.ts.
//!
//! Caller supplies due rows (already filtered to `paused = 0` and
//! `next_fire_at <= now`), the current clock, and a cron→next-time
//! closure. Returns a `TickPlan` of mutations the driver must apply.

use chrono::{DateTime, Utc};

use crate::types::Trigger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvanceOp {
    pub id: String,
    pub next_fire_at: DateTime<Utc>,
    pub last_fire_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseOp {
    pub id: String,
    pub last_error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickPlan {
    /// One-shot ids to delete + fire.
    pub to_delete: Vec<String>,
    /// Recurring rows whose `next_fire_at` advances.
    pub to_advance: Vec<AdvanceOp>,
    /// Triggers with unparseable cron — paused so they leave the due set.
    pub to_pause: Vec<PauseOp>,
    /// Rows the driver dispatches through Fire after the claim txn commits.
    pub to_fire: Vec<Trigger>,
}

/// Decide the mutations for one polling tick.
///
/// Rules:
/// - `kind = 'oneshot'`  → delete + fire.
/// - `kind = 'cron'`, valid expression → advance next_fire_at + fire.
/// - `kind = 'cron'`, bad cron → pause (no fire) so it leaves the due set.
pub fn plan_tick<F>(
    rows: &[Trigger],
    now: DateTime<Utc>,
    compute_next: F,
) -> TickPlan
where
    F: Fn(&str, &str) -> Option<DateTime<Utc>>, // (cron_expr, tz) -> next
{
    let mut plan = TickPlan::default();

    for row in rows {
        if row.is_oneshot() {
            plan.to_delete.push(row.id.clone());
            plan.to_fire.push(row.clone());
            continue;
        }

        // Cron path.
        let expr = match &row.cron_expr {
            Some(e) => e.as_str(),
            None => {
                plan.to_pause.push(PauseOp {
                    id: row.id.clone(),
                    last_error: "cron trigger without expression".into(),
                });
                continue;
            }
        };
        let next = compute_next(expr, &row.timezone);
        if let Some(next_at) = next {
            plan.to_advance.push(AdvanceOp {
                id: row.id.clone(),
                next_fire_at: next_at,
                last_fire_at: now,
            });
            plan.to_fire.push(row.clone());
        } else {
            plan.to_pause.push(PauseOp {
                id: row.id.clone(),
                last_error: "invalid cron".into(),
            });
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn cron_trigger(id: &str, expr: &str) -> Trigger {
        Trigger {
            id: id.into(),
            agent_id: "agent-1".into(),
            kind: "cron".into(),
            cron_expr: Some(expr.into()),
            timezone: "UTC".into(),
            fire_at: None,
            next_fire_at: Utc::now(),
            last_fire_at: None,
            paused: 0,
            last_error: None,
            created_at: Utc::now(),
        }
    }

    fn oneshot_trigger(id: &str) -> Trigger {
        Trigger {
            id: id.into(),
            agent_id: "agent-1".into(),
            kind: "oneshot".into(),
            cron_expr: None,
            timezone: "UTC".into(),
            fire_at: Some(Utc::now()),
            next_fire_at: Utc::now(),
            last_fire_at: None,
            paused: 0,
            last_error: None,
            created_at: Utc::now(),
        }
    }

    fn never(_: &str, _: &str) -> Option<DateTime<Utc>> {
        None
    }

    fn always_in(seconds: i64) -> impl Fn(&str, &str) -> Option<DateTime<Utc>> {
        move |_, _| Some(Utc::now() + Duration::seconds(seconds))
    }

    #[test]
    fn oneshot_is_deleted_and_fired() {
        let rows = vec![oneshot_trigger("o1")];
        let plan = plan_tick(&rows, Utc::now(), never);
        assert_eq!(plan.to_delete, vec!["o1".to_string()]);
        assert_eq!(plan.to_fire.len(), 1);
        assert!(plan.to_advance.is_empty());
        assert!(plan.to_pause.is_empty());
    }

    #[test]
    fn cron_with_valid_expression_advances_and_fires() {
        let rows = vec![cron_trigger("c1", "*/5 * * * *")];
        let plan = plan_tick(&rows, Utc::now(), always_in(300));
        assert!(plan.to_delete.is_empty());
        assert_eq!(plan.to_advance.len(), 1);
        assert_eq!(plan.to_advance[0].id, "c1");
        assert_eq!(plan.to_fire.len(), 1);
        assert!(plan.to_pause.is_empty());
    }

    #[test]
    fn cron_with_unparseable_expression_paused_not_fired() {
        let rows = vec![cron_trigger("c2", "garbage")];
        let plan = plan_tick(&rows, Utc::now(), never);
        assert!(plan.to_fire.is_empty());
        assert_eq!(plan.to_pause.len(), 1);
        assert_eq!(plan.to_pause[0].id, "c2");
        assert_eq!(plan.to_pause[0].last_error, "invalid cron");
    }

    #[test]
    fn cron_without_expression_paused() {
        let mut row = cron_trigger("c3", "");
        row.cron_expr = None;
        let plan = plan_tick(&[row], Utc::now(), always_in(60));
        assert!(plan.to_fire.is_empty());
        assert_eq!(plan.to_pause.len(), 1);
        assert!(plan.to_pause[0].last_error.contains("without expression"));
    }

    #[test]
    fn mixed_batch_partitions_correctly() {
        let rows = vec![
            oneshot_trigger("o1"),
            cron_trigger("c1", "*/5 * * * *"),
            cron_trigger("c-bad", "huh"),
        ];
        // compute_next: only succeed for "*/5 * * * *".
        let plan = plan_tick(&rows, Utc::now(), |expr, _| {
            if expr == "*/5 * * * *" {
                Some(Utc::now() + Duration::minutes(5))
            } else {
                None
            }
        });
        assert_eq!(plan.to_delete, vec!["o1".to_string()]);
        assert_eq!(plan.to_advance.len(), 1);
        assert_eq!(plan.to_pause.len(), 1);
        assert_eq!(plan.to_fire.len(), 2, "oneshot + valid cron fire; bad cron paused");
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler plan::
```

Expected: 5 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/plan.rs
git commit -m "feat(scheduler): planTick pure decision function

Ports claude-agent/lib/scheduler-plan.ts rules: oneshot → delete+fire,
cron+valid → advance+fire, cron+invalid → pause (so it leaves the due
set without hot-looping). Pure function: no I/O, no clock — caller
supplies now and compute_next closure. Five unit tests cover each
branch plus a mixed batch."
```

---

## Task 10: roy_client — Fire over UDS

**Files:**
- Modify: `crates/roy-scheduler/src/roy_client.rs`

A thin async client: connect Unix socket → send one `ClientCommand::Fire` → read frames until terminal `FireDone` / `FireTimeout` / `FireError`. Returns a structured result for the driver.

- [ ] **Step 1: Write the client**

```rust
//! Roy daemon client used by the driver. The only roy import surface
//! allowed in this crate (besides protocol types) is the UDS shape —
//! `ClientCommand` in, `ServerEvent` out, JSON over newline-delimited
//! frames.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use roy::{ClientCommand, FireTarget, ServerEvent, TurnEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Successful Fire result — the turn finished with a terminal Result.
#[derive(Debug, Clone)]
pub struct FireSuccess {
    pub session_id: String,
    pub seq_range: (u64, u64),
    pub cost_usd: Option<f64>,
    pub stop_reason: String,
    pub assistant_text: String,
}

/// Outcome of a Fire call, mapped from the three ServerEvent variants.
#[derive(Debug, Clone)]
pub enum FireOutcome {
    Done(FireSuccess),
    Timeout {
        session_id: String,
        partial_seq_range: (u64, u64),
    },
    Error {
        session_id: Option<String>,
        code: String,
        message: String,
    },
}

pub async fn fire(
    socket_path: &Path,
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
    timeout: Duration,
) -> Result<FireOutcome> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to roy daemon at {}", socket_path.display()))?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let cmd = ClientCommand::Fire {
        target,
        prompt,
        tags,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let line = serde_json::to_string(&cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up before terminal Fire event"))?;
        let evt: ServerEvent = serde_json::from_str(raw.trim())?;
        match evt {
            ServerEvent::FireDone {
                session,
                seq_range,
                result,
                assistant_text,
            } => {
                let TurnEvent::Result { cost_usd, stop_reason } = result else {
                    return Err(anyhow!("non-Result in FireDone"));
                };
                return Ok(FireOutcome::Done(FireSuccess {
                    session_id: session,
                    seq_range,
                    cost_usd,
                    stop_reason: format!("{stop_reason:?}"),
                    assistant_text,
                }));
            }
            ServerEvent::FireTimeout { session, partial_seq_range } => {
                return Ok(FireOutcome::Timeout {
                    session_id: session,
                    partial_seq_range,
                });
            }
            ServerEvent::FireError { session, code, message } => {
                return Ok(FireOutcome::Error {
                    session_id: session,
                    code: code.to_string(),
                    message,
                });
            }
            // Daemon may emit unrelated frames if we share a connection,
            // but for a fresh Fire-only connection there shouldn't be any.
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::net::UnixListener;

    /// Spawn a mock daemon listening on `path` that reads one ClientCommand
    /// and writes one ServerEvent in JSON-line frames.
    async fn spawn_mock(path: std::path::PathBuf, reply: ServerEvent) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _cmd_line = lines.next_line().await.unwrap();
            let out = serde_json::to_string(&reply).unwrap();
            wr.write_all(out.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    #[tokio::test]
    async fn fire_done_maps_to_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::FireDone {
                session: "sid".into(),
                seq_range: (1, 5),
                result: TurnEvent::Result {
                    cost_usd: Some(0.01),
                    stop_reason: roy::StopReason::EndTurn,
                },
                assistant_text: "hi".into(),
            },
        )
        .await;

        let out = fire(
            &path,
            FireTarget::Spawn { preset: "claude".into(), project_id: None },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        match out {
            FireOutcome::Done(s) => {
                assert_eq!(s.session_id, "sid");
                assert_eq!(s.assistant_text, "hi");
                assert_eq!(s.seq_range, (1, 5));
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fire_timeout_maps_to_timeout() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::FireTimeout {
                session: "sid".into(),
                partial_seq_range: (1, 3),
            },
        )
        .await;

        let out = fire(
            &path,
            FireTarget::Spawn { preset: "claude".into(), project_id: None },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(matches!(out, FireOutcome::Timeout { .. }));
    }

    #[tokio::test]
    async fn fire_error_maps_to_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::FireError {
                session: None,
                code: roy::ErrorCode::SpawnFailed,
                message: "boom".into(),
            },
        )
        .await;

        let out = fire(
            &path,
            FireTarget::Spawn { preset: "claude".into(), project_id: None },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(matches!(out, FireOutcome::Error { .. }));
    }

    #[tokio::test]
    async fn no_daemon_at_path_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.sock");
        let r = fire(
            &path,
            FireTarget::Spawn { preset: "claude".into(), project_id: None },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await;
        assert!(r.is_err());
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler roy_client::
```

Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/roy_client.rs
git commit -m "feat(scheduler): roy_client::fire — Fire over Unix socket

Connects to roy daemon, sends ClientCommand::Fire, reads frames until
FireDone/FireTimeout/FireError, maps to FireOutcome enum. Four tests
drive a mock UnixListener: success / timeout / error / no-daemon."
```

---

## Task 11: Subscriber — inject_parent

**Files:**
- Create: `crates/roy-scheduler/src/subscribers/inject_parent.rs`

After a fire finishes, look up the configured parent `session_id`. If the parent is busy mid-turn, `WaitForResult` first (5-min cap). Then send the formatted prompt as a new turn via `ClientCommand::Resume` + `AcquireInput` + `Send` (or via `ClientCommand::Fire` with `FireTarget::Resume`).

Spec §4.1 says config = `{ "session_id": "...", "prefix": "..." }`, `prefix` optional. v1 ships only `format=raw` (silently — no `format` key in v1 config). Per the §4.1 plan refinement: unknown keys are rejected at insert time, so day-1 the config strictly matches.

- [ ] **Step 1: Write the subscriber**

```rust
//! inject_parent subscriber — resume the parent session and send the
//! formatted fire result as the next user turn.
//!
//! Behaviour on parent state:
//! - Live and idle  → send immediately.
//! - Live and busy  → WaitForResult on the parent (5 min cap), then send.
//! - Not live       → SessionNotFound bubbles up as a subscriber error.
//!
//! v1 config:
//!   { "session_id": "<roy session id>", "prefix": "optional string" }

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::roy_client::{self, FireOutcome, FireSuccess};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub session_id: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

pub fn parse_config(json: &str) -> Result<Config> {
    serde_json::from_str(json).context("inject_parent config")
}

pub struct ExecOutcome {
    pub status: &'static str, // "ok" | "error"
    pub error_message: Option<String>,
}

pub async fn execute(
    socket_path: &Path,
    config_json: &str,
    fire_result: &FireSuccess,
) -> ExecOutcome {
    let cfg = match parse_config(config_json) {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("config: {e}")),
            };
        }
    };

    let body = match cfg.prefix {
        Some(p) => format!("{p}{}", fire_result.assistant_text),
        None => fire_result.assistant_text.clone(),
    };

    // Wait for parent to be idle (cheap if it already is), then Fire-Resume
    // to inject. We use Fire here rather than separate Resume + Send so the
    // round-trip is one call and we get an explicit success/timeout/error
    // back from the daemon.
    let outcome = roy_client::fire(
        socket_path,
        roy::FireTarget::Resume {
            session_id: cfg.session_id.clone(),
        },
        body,
        std::collections::BTreeMap::new(),
        Duration::from_secs(5 * 60),
    )
    .await;

    match outcome {
        Ok(FireOutcome::Done(_)) => ExecOutcome {
            status: "ok",
            error_message: None,
        },
        Ok(FireOutcome::Timeout { .. }) => ExecOutcome {
            status: "error",
            error_message: Some("parent stayed busy past 5min".into()),
        },
        Ok(FireOutcome::Error { code, message, .. }) => ExecOutcome {
            status: "error",
            error_message: Some(format!("{code}: {message}")),
        },
        Err(e) => ExecOutcome {
            status: "error",
            error_message: Some(format!("roy_client: {e:#}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    async fn spawn_mock(path: std::path::PathBuf, reply: roy::ServerEvent) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _ = lines.next_line().await.unwrap();
            let out = serde_json::to_string(&reply).unwrap();
            wr.write_all(out.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    fn fake_success() -> FireSuccess {
        FireSuccess {
            session_id: "child-sid".into(),
            seq_range: (0, 5),
            cost_usd: None,
            stop_reason: "EndTurn".into(),
            assistant_text: "the digest".into(),
        }
    }

    #[tokio::test]
    async fn parses_config_with_prefix() {
        let c = parse_config(r#"{"session_id":"sid","prefix":"[bg] "}"#).unwrap();
        assert_eq!(c.session_id, "sid");
        assert_eq!(c.prefix.as_deref(), Some("[bg] "));
    }

    #[tokio::test]
    async fn parses_config_without_prefix() {
        let c = parse_config(r#"{"session_id":"sid"}"#).unwrap();
        assert!(c.prefix.is_none());
    }

    #[tokio::test]
    async fn execute_ok_when_daemon_returns_fire_done() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            roy::ServerEvent::FireDone {
                session: "parent-sid".into(),
                seq_range: (100, 110),
                result: roy::TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: roy::StopReason::EndTurn,
                },
                assistant_text: "".into(),
            },
        )
        .await;

        let out = execute(
            &path,
            r#"{"session_id":"parent-sid"}"#,
            &fake_success(),
        )
        .await;
        assert_eq!(out.status, "ok");
    }

    #[tokio::test]
    async fn execute_error_when_daemon_returns_fire_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            roy::ServerEvent::FireError {
                session: Some("parent-sid".into()),
                code: roy::ErrorCode::NoSession,
                message: "gone".into(),
            },
        )
        .await;

        let out = execute(
            &path,
            r#"{"session_id":"parent-sid"}"#,
            &fake_success(),
        )
        .await;
        assert_eq!(out.status, "error");
        assert!(out.error_message.unwrap().contains("no_session"));
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler subscribers::inject_parent::
```

Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/subscribers/inject_parent.rs
git commit -m "feat(scheduler): inject_parent subscriber

Resume parent session and send formatted fire result as next user turn.
Uses ClientCommand::Fire with FireTarget::Resume — one round-trip,
explicit success/timeout/error mapping. 5-min timeout caps the wait when
parent is busy. Four tests: config parse with/without prefix, end-to-end
ok and error against mock daemons."
```

---

## Task 12: Subscriber — webhook

**Files:**
- Create: `crates/roy-scheduler/src/subscribers/webhook.rs`

Render `body_template` with the placeholder context defined in spec §4.2, POST it, capture status + first 4 KB of response body.

- [ ] **Step 1: Write the template engine + subscriber**

```rust
//! webhook subscriber — render a body template against the fire context
//! and POST it to the configured URL.
//!
//! Template engine: minimal `{{key}}` substitution. No conditionals, no
//! loops, no helpers. Unknown placeholders render as empty string.
//! Authors are responsible for JSON-escaping or any encoding around the
//! placeholders.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::roy_client::FireSuccess;
use crate::types::Fire;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body_template: Option<String>,
}

pub fn parse_config(json: &str) -> Result<Config> {
    serde_json::from_str(json).context("webhook config")
}

/// Spec §4.2 placeholder context. The Fire we get from the store provides
/// the agent-side metadata; the FireSuccess provides the result.
pub fn build_context(
    fire: &Fire,
    agent_name: &str,
    success: Option<&FireSuccess>,
    error_message: Option<&str>,
) -> HashMap<String, String> {
    let mut c = HashMap::new();
    c.insert("agent.id".into(), fire.agent_id.clone());
    c.insert("agent.name".into(), agent_name.into());
    c.insert(
        "trigger.id".into(),
        fire.trigger_id.clone().unwrap_or_default(),
    );
    c.insert("fire.id".into(), fire.id.clone());
    c.insert("fire.started_at".into(), fire.started_at.to_rfc3339());
    c.insert(
        "fire.finished_at".into(),
        fire.finished_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
    );
    let duration_ms = fire
        .finished_at
        .map(|f| (f - fire.started_at).num_milliseconds())
        .unwrap_or(0);
    c.insert("fire.duration_ms".into(), duration_ms.to_string());
    c.insert("fire.status".into(), fire.status.clone());
    c.insert(
        "fire.cost_usd".into(),
        fire.cost_usd.map(|x| x.to_string()).unwrap_or_default(),
    );
    c.insert(
        "fire.stop_reason".into(),
        fire.stop_reason.clone().unwrap_or_default(),
    );
    c.insert(
        "session.id".into(),
        fire.session_id.clone().unwrap_or_default(),
    );
    c.insert(
        "result.assistant_text".into(),
        success
            .map(|s| s.assistant_text.clone())
            .unwrap_or_default(),
    );
    c.insert(
        "result.error_message".into(),
        error_message.unwrap_or("").into(),
    );
    c
}

/// Render `{{key}}` placeholders. Unknown keys render as empty string.
pub fn render(template: &str, ctx: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // find matching }}
            if let Some(end) = find_close(&bytes[i + 2..]) {
                let key = std::str::from_utf8(&bytes[i + 2..i + 2 + end])
                    .unwrap_or("")
                    .trim();
                out.push_str(ctx.get(key).map(String::as_str).unwrap_or(""));
                i += 2 + end + 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn find_close(haystack: &[u8]) -> Option<usize> {
    let mut j = 0;
    while j + 1 < haystack.len() {
        if haystack[j] == b'}' && haystack[j + 1] == b'}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

pub struct ExecOutcome {
    pub status: &'static str,
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

pub async fn execute(
    config_json: &str,
    ctx: &HashMap<String, String>,
) -> ExecOutcome {
    let cfg = match parse_config(config_json) {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("config: {e}")),
                response_snippet: None,
            };
        }
    };

    let body = cfg
        .body_template
        .as_deref()
        .map(|t| render(t, ctx))
        .unwrap_or_default();

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("http client: {e}")),
                response_snippet: None,
            };
        }
    };

    let method = cfg.method.as_deref().unwrap_or("POST").to_uppercase();
    let mut req = match method.as_str() {
        "POST" => client.post(&cfg.url),
        "PUT" => client.put(&cfg.url),
        "PATCH" => client.patch(&cfg.url),
        _ => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("unsupported method: {method}")),
                response_snippet: None,
            };
        }
    };
    for (k, v) in &cfg.headers {
        req = req.header(k, v);
    }
    req = req.body(body);

    match req.send().await {
        Ok(resp) => {
            let status_code = resp.status();
            let snippet = match resp.bytes().await {
                Ok(b) => {
                    let take = b.len().min(4096);
                    Some(String::from_utf8_lossy(&b[..take]).into_owned())
                }
                Err(_) => None,
            };
            if status_code.is_success() {
                ExecOutcome {
                    status: "ok",
                    error_message: None,
                    response_snippet: snippet,
                }
            } else {
                ExecOutcome {
                    status: "error",
                    error_message: Some(format!("HTTP {status_code}")),
                    response_snippet: snippet,
                }
            }
        }
        Err(e) => ExecOutcome {
            status: "error",
            error_message: Some(format!("send: {e}")),
            response_snippet: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ctx_with(text: &str) -> HashMap<String, String> {
        let mut c = HashMap::new();
        c.insert("result.assistant_text".into(), text.into());
        c.insert("agent.name".into(), "digest".into());
        c
    }

    #[test]
    fn render_substitutes_known_keys_and_empties_unknown() {
        let mut ctx = HashMap::new();
        ctx.insert("a".into(), "1".into());
        assert_eq!(render("x={{a}} y={{b}}", &ctx), "x=1 y=");
    }

    #[test]
    fn render_handles_no_placeholders() {
        assert_eq!(render("plain", &HashMap::new()), "plain");
    }

    #[test]
    fn render_handles_unclosed_braces() {
        // `{{a` with no closing — passes through verbatim.
        assert_eq!(render("hi {{a", &HashMap::new()), "hi {{a");
    }

    #[tokio::test]
    async fn execute_posts_rendered_body_to_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let url = format!("{}/hook", server.uri());
        let cfg = format!(
            r#"{{"url":"{url}","body_template":"text={{{{result.assistant_text}}}}"}}"#
        );
        let out = execute(&cfg, &ctx_with("hello")).await;
        assert_eq!(out.status, "ok");
        assert_eq!(out.response_snippet.as_deref(), Some("ok"));

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(String::from_utf8_lossy(&reqs[0].body), "text=hello");
    }

    #[tokio::test]
    async fn execute_records_http_error_with_snippet() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let url = format!("{}/hook", server.uri());
        let cfg = format!(r#"{{"url":"{url}"}}"#);
        let out = execute(&cfg, &ctx_with("x")).await;
        assert_eq!(out.status, "error");
        assert!(out.error_message.unwrap().contains("500"));
        assert_eq!(out.response_snippet.as_deref(), Some("boom"));
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler subscribers::webhook::
```

Expected: 5 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/subscribers/webhook.rs
git commit -m "feat(scheduler): webhook subscriber

Minimal {{key}} template engine (no helpers, no conditionals) + reqwest
POST with first 4KB of response captured for the runs table. Five tests:
template substitution + unknown-key empties + unclosed braces, plus
two against a wiremock MockServer for happy-path POST and 5xx capture."
```

---

## Task 13: Subscriber — notify_native

**Files:**
- Create: `crates/roy-scheduler/src/subscribers/notify_native.rs`

`osascript -e 'display notification "..." with title "..."'` on macOS, `notify-send title body` on Linux. Mocked in tests by injecting the binary path.

- [ ] **Step 1: Write the subscriber**

```rust
//! notify_native subscriber — macOS native notification via osascript,
//! Linux via notify-send. Falls back to a tracing warn on other platforms
//! so the run still reports an outcome.

use std::path::Path;
use std::process::Command;

use anyhow::Context;
use serde::Deserialize;

use crate::roy_client::FireSuccess;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub sound: Option<String>,
}

pub fn parse_config(json: &str) -> anyhow::Result<Config> {
    serde_json::from_str(json).context("notify_native config")
}

pub struct ExecOutcome {
    pub status: &'static str,
    pub error_message: Option<String>,
}

pub fn execute(config_json: &str, agent_name: &str, success: &FireSuccess) -> ExecOutcome {
    let cfg = match parse_config(config_json) {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                status: "error",
                error_message: Some(format!("config: {e}")),
            };
        }
    };
    let title = cfg.title.unwrap_or_else(|| format!("roy-scheduler: {agent_name}"));
    let body = first_line_or_summary(&success.assistant_text);

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            escape_applescript(&body),
            escape_applescript(&title),
        );
        match Command::new("osascript").arg("-e").arg(script).status() {
            Ok(s) if s.success() => return ok(),
            Ok(s) => return err(format!("osascript exited {s}")),
            Err(e) => return err(format!("osascript spawn: {e}")),
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new("notify-send");
        cmd.arg(&title).arg(&body);
        match cmd.status() {
            Ok(s) if s.success() => return ok(),
            Ok(s) => return err(format!("notify-send exited {s}")),
            Err(e) => return err(format!("notify-send spawn: {e}")),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        tracing::warn!(
            target = "roy_scheduler::subscribers::notify_native",
            "no native notifier on this platform; title={title} body={body}"
        );
        ok()
    }
}

fn ok() -> ExecOutcome { ExecOutcome { status: "ok", error_message: None } }
fn err(msg: String) -> ExecOutcome { ExecOutcome { status: "error", error_message: Some(msg) } }

#[allow(dead_code)] // used only when cfg(target_os="macos")
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn first_line_or_summary(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        "(empty)".into()
    } else if line.len() > 200 {
        format!("{}…", &line[..200])
    } else {
        line.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_success(text: &str) -> FireSuccess {
        FireSuccess {
            session_id: "s".into(),
            seq_range: (0, 1),
            cost_usd: None,
            stop_reason: "EndTurn".into(),
            assistant_text: text.into(),
        }
    }

    #[test]
    fn first_line_strips_trailing_chunks() {
        assert_eq!(first_line_or_summary("hello\nworld"), "hello");
    }

    #[test]
    fn first_line_empty_input_yields_placeholder() {
        assert_eq!(first_line_or_summary(""), "(empty)");
    }

    #[test]
    fn first_line_truncates_long_lines() {
        let long: String = std::iter::repeat('x').take(300).collect();
        let out = first_line_or_summary(&long);
        assert!(out.ends_with('…'));
        // 200 'x' + '…' = 201 chars total (in display width). Char count check.
        assert_eq!(out.chars().count(), 201);
    }

    #[test]
    fn escape_applescript_escapes_quotes_and_backslashes() {
        assert_eq!(escape_applescript("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn parse_config_accepts_empty_object() {
        assert!(parse_config("{}").is_ok());
    }

    // No live-execute test — we don't want CI to fire desktop notifications.
    // The cfg-gated platform branch is exercised manually during smoke.
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler subscribers::notify_native::
```

Expected: 5 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/subscribers/notify_native.rs
git commit -m "feat(scheduler): notify_native subscriber

macOS via osascript, Linux via notify-send, other platforms log a warn
and report ok. Title defaults to 'roy-scheduler: <agent>'. Body is the
first line of assistant_text, truncated at 200 chars. Five unit tests
cover the pure helpers; live notification path is manual-smoke only."
```

---

## Task 14: Subscriber dispatch — `src/subscribers/mod.rs`

**Files:**
- Modify: `crates/roy-scheduler/src/subscribers/mod.rs`

Glue: load subscribers for a finished fire, dispatch each to the right module, write `fire_subscriber_runs` rows.

- [ ] **Step 1: Write the dispatcher**

```rust
//! Subscriber dispatcher. Called by `driver::invoke_fire` after a Fire
//! completes. Loads enabled subscribers (agent OR trigger scope), iterates
//! in `order_index ASC, created_at ASC`, executes per-kind, writes a
//! `fire_subscriber_runs` row per attempt. At-most-once per fire — no
//! retry in v1.

use std::path::Path;

use anyhow::Result;
use sqlx::SqlitePool;

use crate::roy_client::FireSuccess;
use crate::store::subscribers as sub_store;
use crate::types::{Fire, Subscriber, SubscriberKind};

pub mod inject_parent;
pub mod notify_native;
pub mod webhook;

pub async fn dispatch(
    pool: &SqlitePool,
    socket_path: &Path,
    fire: &Fire,
    agent_name: &str,
    success: Option<&FireSuccess>,
    error_message: Option<&str>,
) -> Result<()> {
    let subs = sub_store::load_for_fire(pool, &fire.agent_id, fire.trigger_id.as_deref()).await?;

    for sub in subs {
        let kind = match SubscriberKind::parse(&sub.kind) {
            Some(k) => k,
            None => {
                write_run(pool, &sub, "error", Some(format!("unknown kind: {}", sub.kind)), None).await?;
                continue;
            }
        };

        let (status, error, snippet) = match kind {
            SubscriberKind::InjectParent => match success {
                Some(s) => {
                    let out = inject_parent::execute(socket_path, &sub.config, s).await;
                    (out.status, out.error_message, None)
                }
                None => ("skipped", Some("inject_parent skipped (fire did not succeed)".into()), None),
            },
            SubscriberKind::Webhook => {
                let ctx = webhook::build_context(fire, agent_name, success, error_message);
                let out = webhook::execute(&sub.config, &ctx).await;
                (out.status, out.error_message, out.response_snippet)
            }
            SubscriberKind::NotifyNative => match success {
                Some(s) => {
                    let out = notify_native::execute(&sub.config, agent_name, s);
                    (out.status, out.error_message, None)
                }
                None => ("skipped", Some("notify_native skipped (fire did not succeed)".into()), None),
            },
            SubscriberKind::ChainAgent => (
                "error",
                Some("chain_agent: not_implemented in v1".into()),
                None,
            ),
        };

        write_run(pool, &sub, status, error, snippet).await?;
    }

    Ok(())
}

async fn write_run(
    pool: &SqlitePool,
    sub: &Subscriber,
    status: &str,
    error_message: Option<String>,
    response_snippet: Option<String>,
) -> Result<()> {
    sub_store::insert_run(
        pool,
        sub_store::NewSubscriberRun {
            fire_id: "".into(), // filled by caller — see note below
            subscriber_id: sub.id.clone(),
            status: match status {
                "ok" => "ok",
                "skipped" => "skipped",
                _ => "error",
            },
            error_message,
            response_snippet,
        },
    )
    .await?;
    Ok(())
}
```

Wait — `write_run` needs `fire.id`. Let me redo that, threading the fire id through:

Replace `write_run` and the dispatch loop:

```rust
async fn dispatch_impl(/* same args + fire_id */) -> Result<()> {
    // ... in the loop:
    write_run(pool, &fire.id, &sub, status, error, snippet).await?;
}

async fn write_run(
    pool: &SqlitePool,
    fire_id: &str,
    sub: &Subscriber,
    status: &str,
    error_message: Option<String>,
    response_snippet: Option<String>,
) -> Result<()> {
    sub_store::insert_run(
        pool,
        sub_store::NewSubscriberRun {
            fire_id: fire_id.into(),
            subscriber_id: sub.id.clone(),
            status: match status {
                "ok" => "ok",
                "skipped" => "skipped",
                _ => "error",
            },
            error_message,
            response_snippet,
        },
    )
    .await?;
    Ok(())
}
```

(Apply this exact correction when implementing.)

- [ ] **Step 2: Build (no tests yet — covered by driver-level test in Task 15)**

```bash
cargo build -p roy-scheduler --all-targets
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/subscribers/mod.rs
git commit -m "feat(scheduler): subscriber dispatcher

dispatch() loads enabled subscribers for a fire (agent+trigger scope),
runs each per-kind, writes fire_subscriber_runs. chain_agent → not
implemented error. inject_parent / notify_native skip on fire failure.
webhook runs regardless and gets error context."
```

---

## Task 15: Driver — `poll_tick` (single tick)

**Files:**
- Modify: `crates/roy-scheduler/src/driver.rs`

One tick: open claim txn → `select_due` → `plan_tick` → apply mutations → commit → return rows-to-fire.

- [ ] **Step 1: Write the tick fn and its test**

```rust
//! Driver — the polling loop and per-fire invocation. Single-process
//! single-instance (PidLock added in Task 16).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use croner::Cron;
use sqlx::SqlitePool;

use crate::plan::{plan_tick, TickPlan};
use crate::store::triggers;
use crate::types::Trigger;

/// One polling tick: claim due rows in a short transaction, return the
/// rows the caller should dispatch through invoke_fire (OUTSIDE the txn).
pub async fn poll_tick(pool: &SqlitePool, batch_limit: i64) -> Result<Vec<Trigger>> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    let due = triggers::select_due(&mut tx, now, batch_limit).await?;
    let plan = plan_tick(&due, now, compute_next);

    for id in &plan.to_delete {
        triggers::delete_in_txn(&mut tx, id).await?;
    }
    for op in &plan.to_advance {
        triggers::advance_next_fire(&mut tx, &op.id, op.next_fire_at, op.last_fire_at).await?;
    }
    for op in &plan.to_pause {
        triggers::pause(&mut tx, &op.id, &op.last_error).await?;
    }

    tx.commit().await?;
    Ok(plan.to_fire)
}

/// croner-backed `next firing` function used by plan_tick.
fn compute_next(expr: &str, tz: &str) -> Option<DateTime<Utc>> {
    let cron = Cron::new(expr).parse().ok()?;
    let tz: chrono_tz::Tz = tz.parse().ok()?;
    let now = Utc::now().with_timezone(&tz);
    cron.find_next_occurrence(&now, false).ok().map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, store::{agents, triggers as tstore}};
    use chrono::Duration as CDur;
    use tempfile::tempdir;

    #[tokio::test]
    async fn poll_tick_advances_cron_and_returns_to_fire() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(), preset: "claude".into(), project_id: None,
                task: "t".into(), model: None, persistent: false,
            },
        ).await.unwrap();
        let _trig = tstore::insert_cron(
            &pool,
            tstore::NewCronTrigger {
                agent_id: a.id,
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() - CDur::seconds(10),
            },
        ).await.unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert_eq!(to_fire.len(), 1);

        // Second tick: nothing (next_fire_at was advanced).
        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert!(to_fire.is_empty());
    }

    #[tokio::test]
    async fn poll_tick_pauses_bad_cron() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(), preset: "claude".into(), project_id: None,
                task: "t".into(), model: None, persistent: false,
            },
        ).await.unwrap();
        let t = tstore::insert_cron(
            &pool,
            tstore::NewCronTrigger {
                agent_id: a.id,
                cron_expr: "this-is-garbage".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() - CDur::seconds(10),
            },
        ).await.unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert!(to_fire.is_empty());

        let back = tstore::get_by_id(&pool, &t.id).await.unwrap().unwrap();
        assert!(back.is_paused());
        assert_eq!(back.last_error.as_deref(), Some("invalid cron"));
    }

    #[tokio::test]
    async fn poll_tick_deletes_oneshot_and_returns_it() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(), preset: "claude".into(), project_id: None,
                task: "t".into(), model: None, persistent: false,
            },
        ).await.unwrap();
        let t = tstore::insert_oneshot(
            &pool,
            tstore::NewOneshotTrigger {
                agent_id: a.id,
                fire_at: Utc::now() - CDur::seconds(10),
            },
        ).await.unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert_eq!(to_fire.len(), 1);
        assert_eq!(to_fire[0].id, t.id);

        // Trigger is gone after the tick.
        assert!(tstore::get_by_id(&pool, &t.id).await.unwrap().is_none());
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p roy-scheduler driver::tests::poll_tick
```

Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/driver.rs
git commit -m "feat(scheduler): driver::poll_tick — claim-txn + plan + apply

Wraps plan_tick in a short SQLite transaction: select_due, decide, apply
mutations (delete oneshots, advance crons, pause bad ones), commit.
Returns rows the caller should fire OUTSIDE the txn. compute_next uses
croner with per-trigger timezone. Three tests cover oneshot deletion,
cron advancement, and bad-cron pause."
```

---

## Task 16: Driver — `invoke_fire` + `serve` loop with semaphore

**Files:**
- Modify: `crates/roy-scheduler/src/driver.rs`

Glue everything: spawn fires with a bounded semaphore, write the `fires` row, call `roy_client::fire`, update the row, dispatch subscribers. Plus the outer `serve(opts)` loop with PidLock and crash-recovery sweep on startup.

- [ ] **Step 1: Append to `driver.rs`**

Add at the bottom (above `#[cfg(test)] mod tests`):

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::roy_client::{self, FireOutcome};
use crate::store::{agents, fires};
use crate::subscribers;
use crate::types::{Agent, FireStatus, Fire};

#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub db_path: PathBuf,
    pub socket_path: PathBuf,
    pub poll_interval: Duration,
    pub batch_limit: i64,
    pub max_fires: usize,
    pub fire_timeout: Duration,
}

impl Default for ServeOpts {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            socket_path: default_socket_path(),
            poll_interval: Duration::from_millis(1500),
            batch_limit: 50,
            max_fires: 8,
            fire_timeout: Duration::from_secs(600),
        }
    }
}

fn default_db_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SCHEDULER_DB") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-scheduler/state.db")
}

fn default_socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

/// Top-level entry. Opens the DB, runs the crash-recovery sweep, then
/// polls forever. Caller installs the PidLock (see src/main.rs Task 18).
pub async fn serve(opts: ServeOpts) -> Result<()> {
    let pool = crate::db::open(&opts.db_path).await?;

    let swept = fires::sweep_running_older_than(&pool, fires::default_sweep_cutoff()).await?;
    if swept > 0 {
        tracing::warn!(rows = swept, "swept stuck running fires on startup");
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(opts.max_fires));
    let pool = Arc::new(pool);
    let socket_path = Arc::new(opts.socket_path.clone());

    loop {
        match poll_tick(&pool, opts.batch_limit).await {
            Ok(to_fire) => {
                for trig in to_fire {
                    let permit = match Arc::clone(&semaphore).acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let pool = Arc::clone(&pool);
                    let socket_path = Arc::clone(&socket_path);
                    let fire_timeout = opts.fire_timeout;
                    tokio::spawn(async move {
                        if let Err(e) = invoke_fire(&pool, &socket_path, trig, fire_timeout).await {
                            tracing::error!(error = %e, "invoke_fire failed");
                        }
                        drop(permit);
                    });
                }
            }
            Err(e) => tracing::error!(error = %e, "poll_tick failed"),
        }
        tokio::time::sleep(opts.poll_interval).await;
    }
}

pub async fn invoke_fire(
    pool: &SqlitePool,
    socket_path: &std::path::Path,
    trigger: Trigger,
    fire_timeout: Duration,
) -> Result<()> {
    let agent = agents::get_by_id(pool, &trigger.agent_id)
        .await?
        .with_context(|| format!("agent {} missing", trigger.agent_id))?;

    let fire_id = fires::insert_running(
        pool,
        fires::NewFire {
            agent_id: agent.id.clone(),
            trigger_id: Some(trigger.id.clone()),
        },
    )
    .await?;

    let mut tags = BTreeMap::new();
    tags.insert("roy-scheduler:agent_id".into(), agent.id.clone());
    tags.insert("roy-scheduler:trigger_id".into(), trigger.id.clone());
    tags.insert("roy-scheduler:fire_id".into(), fire_id.clone());
    tags.insert("roy-scheduler:kind".into(), "background_fire".into());

    let target = build_target(&agent);
    let outcome = roy_client::fire(socket_path, target, agent.task.clone(), tags, fire_timeout).await;

    let (terminal, success_ref, error_msg) = match outcome {
        Ok(FireOutcome::Done(s)) => (
            fires::TerminalUpdate {
                status: FireStatus::Ok,
                session_id: Some(s.session_id.clone()),
                seq_range: Some((s.seq_range.0 as i64, s.seq_range.1 as i64)),
                assistant_text: Some(s.assistant_text.clone()),
                cost_usd: s.cost_usd,
                stop_reason: Some(s.stop_reason.clone()),
                error_message: None,
            },
            Some(s),
            None,
        ),
        Ok(FireOutcome::Timeout { session_id, partial_seq_range }) => (
            fires::TerminalUpdate {
                status: FireStatus::Timeout,
                session_id: Some(session_id),
                seq_range: Some((partial_seq_range.0 as i64, partial_seq_range.1 as i64)),
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some("fire timed out".into()),
            },
            None,
            Some("fire timed out".to_string()),
        ),
        Ok(FireOutcome::Error { session_id, code, message }) => (
            fires::TerminalUpdate {
                status: FireStatus::Error,
                session_id,
                seq_range: None,
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some(format!("{code}: {message}")),
            },
            None,
            Some(format!("{code}: {message}")),
        ),
        Err(e) => (
            fires::TerminalUpdate {
                status: FireStatus::Error,
                session_id: None,
                seq_range: None,
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some(format!("roy_client: {e:#}")),
            },
            None,
            Some(format!("roy_client: {e:#}")),
        ),
    };

    fires::update_terminal(pool, &fire_id, terminal).await?;

    // If we used Spawn but the agent is persistent, capture the new session id.
    if agent.is_persistent() && agent.persistent_session_id.is_none() {
        if let Some(ref s) = success_ref {
            agents::update_persistent_session_id(pool, &agent.id, Some(&s.session_id)).await?;
        }
    }

    let fire = fires::get_by_id(pool, &fire_id).await?.expect("fire row");
    subscribers::dispatch(
        pool,
        socket_path,
        &fire,
        &agent.name,
        success_ref.as_ref(),
        error_msg.as_deref(),
    )
    .await?;

    Ok(())
}

fn build_target(agent: &Agent) -> roy::FireTarget {
    if agent.is_persistent() {
        if let Some(sid) = agent.persistent_session_id.as_ref() {
            return roy::FireTarget::Resume { session_id: sid.clone() };
        }
    }
    roy::FireTarget::Spawn {
        preset: agent.preset.clone(),
        project_id: agent.project_id.clone(),
    }
}
```

- [ ] **Step 2: Build**

```bash
cargo build -p roy-scheduler --all-targets
cargo test -p roy-scheduler --no-fail-fast
```

Expected: all tests still pass; no new ones added in this task (driver loop integration is the e2e in Task 18).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/src/driver.rs
git commit -m "feat(scheduler): invoke_fire + serve loop

invoke_fire: INSERT fires(running) → tag with roy-scheduler:* keys →
roy_client::fire → UPDATE fires(terminal) → update persistent_session_id
if first-persistent-fire → subscribers::dispatch. serve: open db, crash
sweep (running > 15min → error), bounded-semaphore poll loop forever.
Defaults: 1500ms poll, batch=50, max_fires=8, fire_timeout=10min, db at
~/.local/state/roy-scheduler/state.db."
```

---

## Task 17: CLI — full surface

**Files:**
- Modify: `crates/roy-scheduler/src/main.rs`

Implement every subcommand from spec §5.2 using clap derive. Each is a thin wrapper around the store + driver. PidLock the `serve` subcommand against `~/.local/state/roy-scheduler/serve.pid`.

This is the largest task by line count (~400 LOC of clap glue) — but it's pure boilerplate with no design decisions.

- [ ] **Step 1: Replace `src/main.rs` with the full CLI**

Detail-laden but mechanical: see spec §5.2 for the exact command shapes. Write the file using clap-derive style with subcommand-of-subcommand pattern (`agents add`, `agents list`, etc.). The implementer should match the existing `roy` CLI style for naming/output conventions: JSON line per command output, exit code 0 on success, 1 on agent error, 2 on transport / CLI error.

Top-level structure:

```rust
#[derive(Parser)]
#[command(name = "roy-scheduler")]
struct Cli {
    #[command(subcommand)]
    command: Top,
}

#[derive(Subcommand)]
enum Top {
    Serve(ServeArgs),
    Migrate,
    Agents(AgentsCmd),
    Triggers(TriggersCmd),
    Subscribers(SubscribersCmd),
    Fires(FiresCmd),
    FireNow(FireNowArgs),
}
```

Sub-enums for `AgentsCmd`/`TriggersCmd`/`SubscribersCmd`/`FiresCmd` each carry their own `Add`/`List`/`Show`/`Rm`/etc. variants. Each dispatches to a small `cmd_*` async function that opens the DB pool and calls the relevant store function, printing one JSON line on stdout.

The implementer must adhere to:
- `subscribers add` enforces XOR between `--trigger` and `--agent` via `clap::ArgGroup` (per spec §4 schema constraint).
- `triggers add` validates the cron expression at parse time using croner; rejects with a clear error before insertion.
- `fires show <id>` reads `fires.session_id` and then either reads the live roy journal via `roy::control::ClientCommand::ReadJournal` (which is already a CLI surface in roy-cli — but for roy-scheduler the simplest v1 is to just dump the `fires` row JSON; full journal streaming is "future work").
- `serve` installs a `PidLock` at `~/.local/state/roy-scheduler/serve.pid` (port the PidLock pattern from `crates/roy/src/pid_lock.rs` — it's only ~180 lines and reusable in roy-scheduler with no changes).

This task is too large for a complete code dump in this plan. Implementer should:

1. Write the clap skeleton (Top + sub-enums) and the empty `cmd_*` functions that all return `Ok(())`. Commit.
2. Fill in `agents` subcommands (add/list/show/rm). Commit.
3. Fill in `triggers` subcommands (add/list/rm/pause/resume). Commit.
4. Fill in `subscribers` subcommands (add/list/rm). Commit.
5. Fill in `fires` subcommands (list/show) and `fire-now`. Commit.
6. Fill in `serve` with PidLock and call `driver::serve(opts)`. Commit.

That makes Task 17 effectively 6 commits. Acceptable — each is small and shippable.

- [ ] **Step 2: Build + minimal smoke**

```bash
cargo build --release --bin roy-scheduler
./target/release/roy-scheduler --help
./target/release/roy-scheduler agents --help
./target/release/roy-scheduler triggers --help
./target/release/roy-scheduler subscribers --help
./target/release/roy-scheduler fires --help
```

Each should print clap-formatted help without panic.

```bash
ROY_SCHEDULER_DB=/tmp/rs-smoke.db ./target/release/roy-scheduler migrate
ROY_SCHEDULER_DB=/tmp/rs-smoke.db ./target/release/roy-scheduler agents add \
  --name digest --preset claude --task "summarize"
ROY_SCHEDULER_DB=/tmp/rs-smoke.db ./target/release/roy-scheduler agents list
```

Expect a JSON-line agent record printed.

- [ ] **Step 3: Commit each sub-step**

(See sub-step list above — six commits.)

---

## Task 18: End-to-end smoke test

**Files:**
- Create: `crates/roy-scheduler/tests/e2e.rs`

One `#[ignore]`d integration test that:
1. Spawns `roy serve --socket <tmp>` in the background.
2. Spawns `roy-scheduler serve` against the same socket with a temp DB.
3. Registers an agent + one-shot trigger via the CLI binary.
4. Waits for `fires` row to materialize.
5. Verifies `fires.status == 'ok'` and `assistant_text` is non-empty.

Both binaries point at `python3 tests/scripts/fake-acp-agent.py` via roy's preset-to-binary mapping (we'll use "opencode" since that's how Plan A's tests bind to the fake agent).

- [ ] **Step 1: Write the test**

```rust
//! End-to-end: real roy daemon + real roy-scheduler + fake ACP agent.
//! Ignored by default — runs only when both binaries are built and
//! `cargo test -- --ignored e2e_fire_completes` is requested explicitly.

use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use tempfile::tempdir;

struct DropChild(Child);
impl Drop for DropChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore]
fn e2e_fire_completes() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("roy.sock");
    let db = dir.path().join("scheduler.db");
    let journal = dir.path().join("journals");
    std::fs::create_dir_all(&journal).unwrap();

    // 1. Start roy daemon.
    let _roy = DropChild(
        Command::new(env!("CARGO_BIN_EXE_roy"))
            .arg("serve")
            .arg("--socket").arg(&socket)
            .arg("--journal-dir").arg(&journal)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("roy daemon"),
    );
    sleep(Duration::from_millis(500));

    // 2. roy-scheduler: migrate the DB, then register agent + oneshot trigger.
    let scheduler = env!("CARGO_BIN_EXE_roy-scheduler");
    let common = [
        format!("ROY_SCHEDULER_DB={}", db.display()),
        format!("ROY_SOCKET={}", socket.display()),
    ];

    run(scheduler, &common, &["migrate"]);
    let agent_out = run_capture(scheduler, &common, &[
        "agents", "add",
        "--name", "smoke",
        "--preset", "opencode",
        "--task", "say something",
    ]);
    // Parse agent id from the printed JSON line.
    let v: serde_json::Value = serde_json::from_str(agent_out.trim()).unwrap();
    let agent_id = v["id"].as_str().unwrap().to_string();

    let in_one_sec = chrono::Utc::now() + chrono::Duration::seconds(1);
    run(scheduler, &common, &[
        "triggers", "add",
        "--agent", &agent_id,
        "--oneshot", &in_one_sec.to_rfc3339(),
    ]);

    // 3. Start roy-scheduler serve.
    let _sched = DropChild(
        Command::new(scheduler)
            .env("ROY_SCHEDULER_DB", db.display().to_string())
            .env("ROY_SOCKET", socket.display().to_string())
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("roy-scheduler serve"),
    );

    // 4. Poll fires list until we see one finished.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        let out = run_capture(scheduler, &common, &["fires", "list", "--agent", &agent_id, "--limit", "5"]);
        last = out.clone();
        if out.contains("\"status\":\"ok\"") {
            return; // success
        }
        sleep(Duration::from_millis(500));
    }
    panic!("fire never completed; last fires-list output:\n{last}");
}

fn run(bin: &str, env: &[String], args: &[&str]) {
    let mut cmd = Command::new(bin);
    for kv in env {
        if let Some((k, v)) = kv.split_once('=') {
            cmd.env(k, v);
        }
    }
    let st = cmd.args(args).status().unwrap();
    assert!(st.success(), "{} {:?} exited {st}", bin, args);
}

fn run_capture(bin: &str, env: &[String], args: &[&str]) -> String {
    let mut cmd = Command::new(bin);
    for kv in env {
        if let Some((k, v)) = kv.split_once('=') {
            cmd.env(k, v);
        }
    }
    let out = cmd.args(args).output().unwrap();
    assert!(out.status.success(), "{} {:?} failed:\n{}", bin, args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}
```

- [ ] **Step 2: Verify it runs (manually)**

```bash
cargo build --release --bin roy --bin roy-scheduler
cargo test -p roy-scheduler --test e2e -- --ignored
```

Expected: PASS within ~5 seconds (one cron-trigger fires, fake agent responds with "ack", scheduler records `status=ok`).

If it fails, the most likely culprits are:
- `agents add` doesn't print the new id as JSON — adjust output format in Task 17.
- `roy` doesn't recognise `opencode` for fake-acp-agent — the preset routes to the real `opencode` binary normally; for tests we hijack via `AcpConfig` (look at how `tests/engine.rs` does it in Plan A — same trick may need a flag in roy serve to point preset → fake script). Implementer should resolve.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-scheduler/tests/e2e.rs
git commit -m "test(scheduler): #[ignore]d end-to-end via real roy + scheduler

Spawns roy daemon + roy-scheduler serve against a temp socket and DB,
registers an agent + oneshot trigger via CLI, polls fires list until
status=ok or 30s timeout. Run with: cargo test -p roy-scheduler --test
e2e -- --ignored e2e_fire_completes."
```

---

## Wrap-up

After Task 18:

- [ ] **Final CI gate**

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```

All three must pass. The e2e test in Task 18 is `#[ignore]`d so the default `cargo test` doesn't run it.

- [ ] **Update root README.md or docs/scheduler.md**

Add a section to `README.md` (or create `docs/scheduler.md` referenced from README) describing:
- What roy-scheduler does (one paragraph)
- Where the DB lives (`~/.local/state/roy-scheduler/state.db`)
- Example: `roy-scheduler agents add --name digest --preset claude --task "summarize today" && roy-scheduler triggers add --agent <id> --cron '0 9 * * *'`
- Reference to the design spec and this plan

- [ ] **Hand off**

The branch is now ready for the final-reviewer pass (per superpowers:subagent-driven-development) and then `superpowers:finishing-a-development-branch` for merge.

---

## Self-Review

(Performed by the plan author before handing this document to implementers.)

**Spec coverage:** §4 schema → Tasks 3-8. §5 driver → Tasks 9-16. §6 PG-readiness → Cargo deps (sqlx) + parallel migrations dir (Task 3). §7 failure modes → sweep + subscriber error handling. §8 testing strategy → per-task unit tests + e2e in Task 18.

**Placeholders:** none I can spot. The "Task 17 implementer should split into 6 commits" is a directive, not a placeholder.

**Type consistency:** `FireSuccess` defined in Task 10, used by 11/12/13/14/16. `Subscriber`/`Fire`/`Agent` from Task 4 used everywhere. `FireOutcome` enum from Task 10 used in 11 + 16. `SubscriberKind` from Task 4 used in 8 + 14. All match.

**Scope:** 18 tasks (counting Task 17's 6 sub-commits as one). Per-task work is bite-sized. The ones that risk going long are Task 12 (webhook with template engine + wiremock) and Task 17 (CLI surface) — both flagged with sub-commit guidance.

**Open items:**
- Task 18's e2e test depends on `roy serve` accepting an option to point a preset at a custom fake-agent binary. If it currently doesn't, the implementer needs a small roy-side change (or pass tags through to override transport). Surface to user when this comes up.
- `cron_trigger` validation at CLI insert time (Task 17) needs croner to accept the same expression that `compute_next` will evaluate later — verify they agree on syntax (5-field vs 6-field).
