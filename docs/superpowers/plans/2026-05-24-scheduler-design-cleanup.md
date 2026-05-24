# Scheduler Design Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tighten five concrete abstractions inside `roy-scheduler` (Subscriber trait + 4 mechanical fixes) without changing external behaviour. Wire protocol, DB schema, and CLI surface stay bit-for-bit identical.

**Architecture:** Five commits, each stand-alone-green under CI. Commits ordered by graph dependency (PidLock → TriggerKind → ChainAgent → Subscriber trait → CLAUDE.md), not by impact. Spec: `docs/superpowers/specs/2026-05-24-scheduler-design-cleanup.md`.

**Tech Stack:** Rust 2021 (workspace), tokio, sqlx (SQLite), `async_trait`, `OnceLock`. Test harness: `cargo test --workspace --no-fail-fast`, integration tests use `tempfile`, `wiremock`, and `tokio::net::UnixListener` mocks.

**Branch:** `refactor/scheduler-design-cleanup` (already created at HEAD `69d6908` with the design spec).

---

## Task 1: PidLock dedup

**Files:**
- Delete: `crates/roy-scheduler/src/pid_lock.rs`
- Modify: `crates/roy-scheduler/src/main.rs` (drop `mod pid_lock;`, change `use` path)

`roy::PidLock` is already publicly exported (`crates/roy/src/lib.rs:9,22`) and `roy-scheduler` already depends on `roy` for protocol types. No `Cargo.toml` edit needed.

- [ ] **Step 1: Verify the two PidLock APIs are interchangeable**

Read both files and confirm they expose the same public surface used by `main.rs`:

```bash
diff <(grep -E "^\s*pub (fn|struct|impl|use)" /Users/i_strelov/Projects/roy/crates/roy/src/pid_lock.rs) \
     <(grep -E "^\s*pub (fn|struct|impl|use)" /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/pid_lock.rs)
```

Expected: the public surfaces match (both expose `PidLock`, `PidLock::acquire`, Drop impl). Differences in private helpers are fine.

- [ ] **Step 2: Find the call sites in roy-scheduler**

```bash
grep -rn "pid_lock\|PidLock" /Users/i_strelov/Projects/roy/crates/roy-scheduler/
```

Expected: hits in `main.rs` only (a `mod pid_lock;` declaration + a `pid_lock::PidLock::acquire(...)` call inside `cmd_serve`).

- [ ] **Step 3: Replace the import in main.rs**

In `crates/roy-scheduler/src/main.rs`, remove the `mod pid_lock;` line near the top (currently `main.rs:14`):

```rust
// REMOVE this line:
mod pid_lock;
```

In the `cmd_serve` function, change the `PidLock::acquire(&pid_path)` call site so it uses `roy::PidLock` instead of the local module:

```rust
let _lock = roy::PidLock::acquire(&pid_path)
    .with_context(|| format!("acquiring pid lock at {}", pid_path.display()))?;
```

- [ ] **Step 4: Delete the duplicate file**

```bash
rm /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/pid_lock.rs
```

- [ ] **Step 5: Build the workspace**

```bash
cargo build --workspace --all-targets
```

Expected: success, no warnings about unused imports.

- [ ] **Step 6: Run the full test suite**

```bash
cargo test --workspace --no-fail-fast
```

Expected: all tests pass (including any `pid_lock` tests that lived in `crates/roy/src/pid_lock.rs` — those stay untouched).

- [ ] **Step 7: Commit**

```bash
git add crates/roy-scheduler/src/main.rs crates/roy-scheduler/src/pid_lock.rs
git commit -m "$(cat <<'EOF'
chore(roy): expose PidLock; drop roy-scheduler copy

The two implementations were 1:1 with the same public surface; roy
already exports PidLock via crates/roy/src/lib.rs. Dedup -147 LOC.
EOF
)"
```

---

## Task 2: TriggerKind enum

**Files:**
- Modify: `crates/roy-scheduler/src/types.rs` (add enum, change `Trigger::is_oneshot`)
- Modify: `crates/roy-scheduler/src/plan.rs` (the one external `.kind == "oneshot"` site, if present)

**Design choice (locked in the spec):** the DB column stays `TEXT`. We add a typed accessor `Trigger::kind() -> TriggerKind` and keep the raw `String` field for `FromRow` compatibility. Call sites stop comparing string literals.

- [ ] **Step 1: Find current string-comparison sites**

```bash
grep -rn '\.kind == "\|kind: String' /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/
```

Expected hits: `types.rs:34` (the struct field), `types.rs:53` (`is_oneshot` uses `self.kind == "oneshot"`). If `plan.rs` or any other site grep'd hits, note them — they need migration to `is_oneshot()` in Step 5.

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/roy-scheduler/src/types.rs`:

```rust
#[test]
fn trigger_kind_roundtrips() {
    for kind in [TriggerKind::Cron, TriggerKind::Oneshot] {
        assert_eq!(TriggerKind::parse(kind.as_db()), Some(kind));
    }
    assert_eq!(TriggerKind::parse("nope"), None);
}
```

- [ ] **Step 3: Run the test, see it fail**

```bash
cargo test -p roy-scheduler --lib types::tests::trigger_kind_roundtrips
```

Expected: compile error — `TriggerKind` does not exist.

- [ ] **Step 4: Add the enum**

In `crates/roy-scheduler/src/types.rs`, add this block just above the existing `pub struct Trigger { ... }` definition (around line 32):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Cron,
    Oneshot,
}

impl TriggerKind {
    pub fn as_db(self) -> &'static str {
        match self {
            TriggerKind::Cron => "cron",
            TriggerKind::Oneshot => "oneshot",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cron" => Some(Self::Cron),
            "oneshot" => Some(Self::Oneshot),
            _ => None,
        }
    }
}
```

- [ ] **Step 5: Rewrite `is_oneshot` to go through the enum**

In `crates/roy-scheduler/src/types.rs`, replace the existing `is_oneshot` body inside `impl Trigger { ... }`:

```rust
impl Trigger {
    pub fn is_paused(&self) -> bool {
        self.paused != 0
    }

    pub fn kind(&self) -> TriggerKind {
        TriggerKind::parse(&self.kind)
            .unwrap_or_else(|| panic!("invalid kind in DB: {:?}", self.kind))
    }

    pub fn is_oneshot(&self) -> bool {
        matches!(self.kind(), TriggerKind::Oneshot)
    }
}
```

Keep the `pub kind: String` field on `Trigger` unchanged — `FromRow` needs it.

- [ ] **Step 6: Migrate any external string-comparison call site**

Grep again:

```bash
grep -rn '\.kind == "\|trigger\.kind' /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/ | grep -v types.rs
```

If `plan.rs` (or any other file) compares `.kind` to a string literal, replace with `.is_oneshot()` or `matches!(t.kind(), TriggerKind::Oneshot)`. If grep returns zero non-`types.rs` hits, this step is a no-op.

- [ ] **Step 7: Run the test, see it pass**

```bash
cargo test -p roy-scheduler --lib types::tests::trigger_kind_roundtrips
```

Expected: PASS.

- [ ] **Step 8: Run the full workspace test**

```bash
cargo test --workspace --no-fail-fast
```

Expected: all tests pass, including `poll_tick_deletes_oneshot_and_returns_it` (which exercises `is_oneshot` via the driver).

- [ ] **Step 9: Commit**

```bash
git add crates/roy-scheduler/src/types.rs crates/roy-scheduler/src/plan.rs
git commit -m "$(cat <<'EOF'
refactor(scheduler): TriggerKind enum, drop kind: String comparisons

DB column stays TEXT; Trigger::kind() is the typed boundary. Call
sites use is_oneshot() instead of comparing string literals.
EOF
)"
```

(If `plan.rs` had no changes, omit it from the `git add`.)

---

## Task 3: Remove `SubscriberKind::ChainAgent`

**Files:**
- Modify: `crates/roy-scheduler/src/types.rs` (drop variant + `as_db`/`parse` arms + test iteration)
- Modify: `crates/roy-scheduler/src/subscribers/mod.rs` (drop `ChainAgent` arm in dispatch)

- [ ] **Step 1: Update the existing roundtrip test**

In `crates/roy-scheduler/src/types.rs`, the test `subscriber_kind_roundtrips` currently iterates four variants. Remove `SubscriberKind::ChainAgent` from the array literal:

```rust
#[test]
fn subscriber_kind_roundtrips() {
    for kind in [
        SubscriberKind::InjectParent,
        SubscriberKind::Webhook,
        SubscriberKind::NotifyNative,
    ] {
        assert_eq!(SubscriberKind::parse(kind.as_db()), Some(kind));
    }
    assert_eq!(SubscriberKind::parse("nope"), None);
}
```

- [ ] **Step 2: Run the test, see it fail to compile**

```bash
cargo test -p roy-scheduler --lib types::tests::subscriber_kind_roundtrips
```

Expected: compile error in the dispatch site referencing `SubscriberKind::ChainAgent` — that's the next step.

(The test itself compiles fine; the error will come from `subscribers/mod.rs` because the match becomes non-exhaustive once we delete the variant.)

- [ ] **Step 3: Remove the variant from the enum**

In `crates/roy-scheduler/src/types.rs`, delete `ChainAgent` from `enum SubscriberKind`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriberKind {
    InjectParent,
    Webhook,
    NotifyNative,
}
```

And drop the `ChainAgent` arms from `as_db` and `parse`:

```rust
impl SubscriberKind {
    pub fn as_db(self) -> &'static str {
        match self {
            SubscriberKind::InjectParent => "inject_parent",
            SubscriberKind::Webhook => "webhook",
            SubscriberKind::NotifyNative => "notify_native",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inject_parent" => Some(Self::InjectParent),
            "webhook" => Some(Self::Webhook),
            "notify_native" => Some(Self::NotifyNative),
            _ => None,
        }
    }
}
```

- [ ] **Step 4: Drop the dispatch arm**

In `crates/roy-scheduler/src/subscribers/mod.rs`, the `match kind { ... }` inside `dispatch` has a `SubscriberKind::ChainAgent => (...)` arm (lines ~75-80). Delete that arm. The compiler now confirms the match is exhaustive over the three remaining variants.

- [ ] **Step 5: Run the workspace tests**

```bash
cargo test --workspace --no-fail-fast
```

Expected: all pass. `chain_agent` strings should not appear in any compiled binary.

- [ ] **Step 6: Sanity-check that ChainAgent is gone**

```bash
grep -rn "ChainAgent\|chain_agent" /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/
```

Expected: zero hits.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-scheduler/src/types.rs crates/roy-scheduler/src/subscribers/mod.rs
git commit -m "$(cat <<'EOF'
refactor(scheduler): remove SubscriberKind::ChainAgent

The variant was a permanent "not_implemented in v1" stub. Existing DB
rows with kind='chain_agent' now flow through the unknown-kind branch
in dispatch and produce a fire_subscriber_runs row with status='error'
— strictly clearer than the previous hardcoded message.
EOF
)"
```

---

## Task 4: Subscriber trait + registry

This is the only architectural commit. Done as one logical change but broken into TDD steps. Each subscriber gets converted independently to keep diffs small.

**Files:**
- Modify: `crates/roy-scheduler/src/subscribers/mod.rs` (add trait, `FireCtx`, `Outcome`; restructure dispatch)
- Create: `crates/roy-scheduler/src/subscribers/registry.rs`
- Create: `crates/roy-scheduler/src/subscribers/dispatch.rs`
- Modify: `crates/roy-scheduler/src/subscribers/inject_parent.rs` (impl Subscriber)
- Modify: `crates/roy-scheduler/src/subscribers/webhook.rs` (impl Subscriber)
- Modify: `crates/roy-scheduler/src/subscribers/notify_native.rs` (impl Subscriber)

### 4a: Establish the trait, types, and skeleton registry

- [ ] **Step 1: Write the failing registry test**

Create `crates/roy-scheduler/src/subscribers/registry.rs` with:

```rust
//! Static registry of SubscriberKind → ctor. Each kind builds a
//! `Box<dyn Subscriber>` from a JSON config string.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;

use super::Subscriber;
use crate::types::SubscriberKind;

pub type SubscriberCtor = fn(config_json: &str) -> Result<Box<dyn Subscriber>>;

pub fn registry() -> &'static HashMap<SubscriberKind, SubscriberCtor> {
    static R: OnceLock<HashMap<SubscriberKind, SubscriberCtor>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m: HashMap<SubscriberKind, SubscriberCtor> = HashMap::new();
        m.insert(SubscriberKind::InjectParent, super::inject_parent::build);
        m.insert(SubscriberKind::Webhook, super::webhook::build);
        m.insert(SubscriberKind::NotifyNative, super::notify_native::build);
        m
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_registered() {
        for kind in [
            SubscriberKind::InjectParent,
            SubscriberKind::Webhook,
            SubscriberKind::NotifyNative,
        ] {
            assert!(
                registry().contains_key(&kind),
                "registry missing ctor for {:?}",
                kind
            );
        }
    }
}
```

- [ ] **Step 2: Run the test, see it fail to compile**

```bash
cargo test -p roy-scheduler --lib subscribers::registry::tests::all_kinds_registered
```

Expected: compile errors — `Subscriber` trait does not exist, neither do `inject_parent::build`, `webhook::build`, `notify_native::build`.

- [ ] **Step 3: Add the trait, FireCtx, Outcome, RunStatus to mod.rs**

At the top of `crates/roy-scheduler/src/subscribers/mod.rs`, after the existing imports, add the trait and supporting types:

```rust
use async_trait::async_trait;

pub mod dispatch;
pub mod inject_parent;
pub mod notify_native;
pub mod registry;
pub mod webhook;

#[async_trait]
pub trait Subscriber: Send + Sync {
    async fn run(&self, ctx: &FireCtx<'_>) -> Outcome;
}

pub struct FireCtx<'a> {
    pub pool: &'a sqlx::SqlitePool,
    pub socket_path: &'a std::path::Path,
    pub fire: &'a crate::types::Fire,
    pub agent_name: &'a str,
    pub success: Option<&'a crate::roy_client::FireSuccess>,
    pub error_message: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Ok,
    Error,
    Skipped,
}

impl RunStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Error => "error",
            RunStatus::Skipped => "skipped",
        }
    }
}

pub struct Outcome {
    pub status: RunStatus,
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

impl Outcome {
    pub fn ok() -> Self {
        Self { status: RunStatus::Ok, error_message: None, response_snippet: None }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self { status: RunStatus::Error, error_message: Some(msg.into()), response_snippet: None }
    }
    pub fn skipped(msg: impl Into<String>) -> Self {
        Self { status: RunStatus::Skipped, error_message: Some(msg.into()), response_snippet: None }
    }
}
```

NOTE: the existing `pub async fn dispatch(...)` in `mod.rs` stays as-is for now — we'll move it to `dispatch.rs` in step 4d. Both versions coexist briefly while subscribers are converted, then the old `dispatch` is removed.

- [ ] **Step 4: Run the test, see new errors about `build` symbols**

```bash
cargo test -p roy-scheduler --lib subscribers::registry::tests::all_kinds_registered
```

Expected: `Subscriber` is now defined, but `inject_parent::build` / `webhook::build` / `notify_native::build` are still missing. That's the next sub-task.

### 4b: Convert `inject_parent`

- [ ] **Step 5: Refactor `execute` to accept `&Config` directly**

In `crates/roy-scheduler/src/subscribers/inject_parent.rs`, change the existing `execute` signature so the caller (next step) doesn't have to re-serialize. Replace the function header from:

```rust
pub async fn execute(
    socket_path: &Path,
    config_json: &str,
    fire_result: &FireSuccess,
) -> ExecOutcome {
    let cfg = match parse_config(config_json) {
        Ok(c) => c,
        Err(e) => { return ExecOutcome { status: "error", error_message: Some(format!("config: {e}")) }; }
    };
    // ... existing body uses `cfg` ...
```

To:

```rust
pub async fn execute(
    socket_path: &Path,
    cfg: &Config,
    fire_result: &FireSuccess,
) -> ExecOutcome {
    // ... existing body uses `cfg` (now passed in) ...
```

Update the existing tests in the same file: each call like `execute(&path, "{...json...}", &success)` becomes `execute(&path, &parse_config("{...json...}").unwrap(), &success)`. Two tests need this change (`execute_ok_when_daemon_returns_fire_done`, `execute_error_when_daemon_returns_fire_error`).

- [ ] **Step 6: Add `build` and `impl Subscriber`**

Append to `crates/roy-scheduler/src/subscribers/inject_parent.rs`:

```rust
use async_trait::async_trait;
use super::{FireCtx, Outcome, Subscriber};

pub fn build(config_json: &str) -> anyhow::Result<Box<dyn Subscriber>> {
    let cfg = parse_config(config_json)?;
    Ok(Box::new(InjectParentSubscriber { cfg }))
}

pub struct InjectParentSubscriber {
    cfg: Config,
}

#[async_trait]
impl Subscriber for InjectParentSubscriber {
    async fn run(&self, ctx: &FireCtx<'_>) -> Outcome {
        let Some(success) = ctx.success else {
            return Outcome::skipped("inject_parent skipped (fire did not succeed)");
        };
        let exec = execute(ctx.socket_path, &self.cfg, success).await;
        match exec.status {
            "ok" => Outcome::ok(),
            _ => Outcome::error(exec.error_message.unwrap_or_else(|| "inject_parent failed".into())),
        }
    }
}
```

- [ ] **Step 7: Run the file's tests**

```bash
cargo test -p roy-scheduler --lib subscribers::inject_parent
```

Expected: existing two tests pass with the updated call shape `execute(path, &parse_config(...).unwrap(), &success)`.

- [ ] **Step 8: Run the registry test**

```bash
cargo test -p roy-scheduler --lib subscribers::registry::tests::all_kinds_registered
```

Expected: still failing — `webhook::build` and `notify_native::build` are not yet defined. Next sub-task.

### 4c: Convert `webhook`

- [ ] **Step 9: Add `build` and `impl Subscriber` to webhook.rs**

The webhook execute is shaped `execute(config_json: &str, ctx: &HashMap<String, String>) -> ExecOutcome`. The `ctx` is built by `build_context(fire, agent_name, success, error_message)` — that helper stays as the per-run input.

At the bottom of `crates/roy-scheduler/src/subscribers/webhook.rs`, add:

```rust
use async_trait::async_trait;
use super::{FireCtx, Outcome, Subscriber};

pub fn build(config_json: &str) -> anyhow::Result<Box<dyn Subscriber>> {
    let cfg = parse_config(config_json)?;
    Ok(Box::new(WebhookSubscriber { cfg }))
}

pub struct WebhookSubscriber {
    cfg: Config,
}

#[async_trait]
impl Subscriber for WebhookSubscriber {
    async fn run(&self, ctx: &FireCtx<'_>) -> Outcome {
        let render_ctx = build_context(ctx.fire, ctx.agent_name, ctx.success, ctx.error_message);
        let exec = execute_with_cfg(&self.cfg, &render_ctx).await;
        match exec.status {
            "ok" => Outcome {
                status: super::RunStatus::Ok,
                error_message: exec.error_message,
                response_snippet: exec.response_snippet,
            },
            _ => Outcome {
                status: super::RunStatus::Error,
                error_message: exec.error_message,
                response_snippet: exec.response_snippet,
            },
        }
    }
}
```

Refactor the existing `execute(config_json, ctx)` into `execute_with_cfg(cfg: &Config, ctx)`:

  - Rename existing `pub async fn execute(config_json: &str, ctx: &HashMap<String, String>) -> ExecOutcome` to `pub async fn execute_with_cfg(cfg: &Config, ctx: &HashMap<String, String>) -> ExecOutcome`.
  - Remove the `parse_config(config_json)` call from inside; the caller has the `&Config`.
  - Keep a tiny `pub async fn execute(config_json: &str, ctx: &HashMap<String, String>) -> ExecOutcome` wrapper IF the existing tests call `execute` by the old name — easier than rewriting all tests. Check with `grep "execute(" crates/roy-scheduler/src/subscribers/webhook.rs`; if the existing tests call `execute`, leave the wrapper; if they call `execute_with_cfg`, drop the wrapper.

Likely the tests call `execute`, so keep the wrapper:

```rust
pub async fn execute(config_json: &str, ctx: &HashMap<String, String>) -> ExecOutcome {
    let cfg = match parse_config(config_json) {
        Ok(c) => c,
        Err(e) => return ExecOutcome { status: "error", error_message: Some(format!("config: {e}")), response_snippet: None },
    };
    execute_with_cfg(&cfg, ctx).await
}
```

- [ ] **Step 10: Run the file's tests**

```bash
cargo test -p roy-scheduler --lib subscribers::webhook
```

Expected: existing tests (`execute_posts_rendered_body_to_url`, `execute_records_http_error_with_snippet`) pass.

### 4d: Convert `notify_native`

- [ ] **Step 11: Add `build` and `impl Subscriber` to notify_native.rs**

Same shape as inject_parent. Refactor the existing `execute` to accept `&Config` directly, then add:

```rust
use async_trait::async_trait;
use super::{FireCtx, Outcome, Subscriber};

pub fn build(config_json: &str) -> anyhow::Result<Box<dyn Subscriber>> {
    let cfg = parse_config(config_json)?;
    Ok(Box::new(NotifyNativeSubscriber { cfg }))
}

pub struct NotifyNativeSubscriber {
    cfg: Config,
}

#[async_trait]
impl Subscriber for NotifyNativeSubscriber {
    async fn run(&self, ctx: &FireCtx<'_>) -> Outcome {
        let Some(success) = ctx.success else {
            return Outcome::skipped("notify_native skipped (fire did not succeed)");
        };
        let exec = execute_with_cfg(&self.cfg, ctx.agent_name, success).await;
        match exec.status {
            "ok" => Outcome::ok(),
            _ => Outcome::error(exec.error_message.unwrap_or_else(|| "notify_native failed".into())),
        }
    }
}
```

Refactor existing `execute(config_json, agent_name, success)` → `execute_with_cfg(cfg, agent_name, success)`, keeping the old name as a thin wrapper if any existing tests need it (check with grep — same approach as webhook).

- [ ] **Step 12: Run the file's tests**

```bash
cargo test -p roy-scheduler --lib subscribers::notify_native
```

Expected: all pass.

- [ ] **Step 13: Run the registry test — should pass now**

```bash
cargo test -p roy-scheduler --lib subscribers::registry::tests::all_kinds_registered
```

Expected: PASS. All three ctors exist.

### 4e: Move dispatch to its own file via the registry

- [ ] **Step 14: Write the failing dispatch test**

Create `crates/roy-scheduler/src/subscribers/dispatch.rs` with the new dispatch implementation AND an inline test for the unknown-kind path:

```rust
//! Subscriber dispatcher. Loads enabled subscribers for a fire, builds each
//! via the registry, runs them in order, and writes one fire_subscriber_runs
//! row per attempt. At-most-once per fire — no retry in v1.

use std::path::Path;

use anyhow::Result;
use sqlx::SqlitePool;

use super::registry::registry;
use super::{Outcome, RunStatus, Subscriber};
use crate::roy_client::FireSuccess;
use crate::store::subscribers as sub_store;
use crate::types::{Fire, Subscriber as SubscriberRow, SubscriberKind};

pub async fn dispatch(
    pool: &SqlitePool,
    socket_path: &Path,
    fire: &Fire,
    agent_name: &str,
    success: Option<&FireSuccess>,
    error_message: Option<&str>,
) -> Result<()> {
    let subs = sub_store::load_for_fire(pool, &fire.agent_id, fire.trigger_id.as_deref()).await?;

    let ctx = super::FireCtx {
        pool,
        socket_path,
        fire,
        agent_name,
        success,
        error_message,
    };

    for sub_row in subs {
        let outcome = run_one(&sub_row, &ctx).await;
        write_run(pool, &fire.id, &sub_row, outcome).await?;
    }

    Ok(())
}

async fn run_one(sub_row: &SubscriberRow, ctx: &super::FireCtx<'_>) -> Outcome {
    let Some(kind) = SubscriberKind::parse(&sub_row.kind) else {
        return Outcome::error(format!("unknown kind: {}", sub_row.kind));
    };
    let Some(ctor) = registry().get(&kind) else {
        return Outcome::error(format!("kind not registered: {}", sub_row.kind));
    };
    match ctor(&sub_row.config) {
        Ok(sub) => sub.run(ctx).await,
        Err(e) => Outcome::error(format!("config parse: {e:#}")),
    }
}

async fn write_run(
    pool: &SqlitePool,
    fire_id: &str,
    sub: &SubscriberRow,
    outcome: Outcome,
) -> Result<()> {
    sub_store::insert_run(
        pool,
        sub_store::NewSubscriberRun {
            fire_id: fire_id.into(),
            subscriber_id: sub.id.clone(),
            status: outcome.status.as_db(),
            error_message: outcome.error_message,
            response_snippet: outcome.response_snippet,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::store::{agents, subscribers as sub_store};
    use tempfile::tempdir;

    #[tokio::test]
    async fn unknown_kind_writes_error_run() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
            },
        )
        .await
        .unwrap();
        // Insert a subscriber row with an unknown kind directly via SQL so we
        // bypass SubscriberKind::parse on the insert path.
        let sub_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO fire_subscribers (id, agent_id, kind, config, enabled, order_index, created_at) \
             VALUES (?, ?, 'bogus', '{}', 1, 0, datetime('now'))",
        )
        .bind(&sub_id)
        .bind(&a.id)
        .execute(&pool)
        .await
        .unwrap();

        // Fake a Fire row.
        let fire_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO fires (id, agent_id, status, started_at) \
             VALUES (?, ?, 'ok', datetime('now'))",
        )
        .bind(&fire_id)
        .bind(&a.id)
        .execute(&pool)
        .await
        .unwrap();
        let fire = crate::store::fires::get_by_id(&pool, &fire_id).await.unwrap().unwrap();

        dispatch(&pool, std::path::Path::new("/unused"), &fire, "agent", None, None)
            .await
            .unwrap();

        let runs = sub_store::list_runs_for_fire(&pool, &fire.id).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "error");
        assert!(runs[0]
            .error_message
            .as_deref()
            .unwrap()
            .contains("unknown kind"));
    }
}
```

NOTE: the test imports `crate::types::Subscriber as SubscriberRow` because `Subscriber` is now both a trait (in `subscribers::Subscriber`) and a DB row type (in `types::Subscriber`). Rename in the test to avoid the clash.

The two-name conflict also affects production code: in `dispatch.rs` we `use crate::types::{Fire, Subscriber as SubscriberRow, SubscriberKind}` to avoid colliding with `super::Subscriber` (the trait).

- [ ] **Step 15: Run the dispatch test, see it fail**

```bash
cargo test -p roy-scheduler --lib subscribers::dispatch::tests::unknown_kind_writes_error_run
```

Expected: compile errors — both `mod.rs::dispatch` (old) and `dispatch.rs::dispatch` (new) exist. Resolve by removing the old `pub async fn dispatch` from `mod.rs`.

- [ ] **Step 16: Remove the old dispatch from mod.rs**

In `crates/roy-scheduler/src/subscribers/mod.rs`, delete the existing `pub async fn dispatch(...)` and its private `write_run` helper. Replace the public re-export with:

```rust
pub use dispatch::dispatch;
```

Keep the trait, `FireCtx`, `Outcome`, `RunStatus` definitions in `mod.rs`.

- [ ] **Step 17: Run the dispatch test, see it pass**

```bash
cargo test -p roy-scheduler --lib subscribers::dispatch::tests::unknown_kind_writes_error_run
```

Expected: PASS.

- [ ] **Step 18: Verify no caller of `dispatch` broke**

```bash
grep -rn "subscribers::dispatch" /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/
```

Expected: hits in `driver.rs` (the caller from `run_fire_for_agent`). The signature is unchanged, so no edit needed.

- [ ] **Step 19: Run the whole workspace test suite**

```bash
cargo test --workspace --no-fail-fast
```

Expected: every test passes, including:
- `driver::tests::fire_agent_ad_hoc_dispatches_subscribers` (end-to-end through the new dispatch)
- `tests/e2e.rs` (whole-system check, requires `python3`)

- [ ] **Step 20: Format check**

```bash
cargo fmt --all -- --check
```

Expected: no diffs. If there are, run `cargo fmt --all` and stage the changes.

- [ ] **Step 21: Commit**

```bash
git add crates/roy-scheduler/src/subscribers/
git commit -m "$(cat <<'EOF'
refactor(scheduler): Subscriber trait + registry

Replaces the per-kind match in subscribers::dispatch with a static
SubscriberKind→ctor registry returning Box<dyn Subscriber>. Skip-on-
non-success semantics move into the implementations that need them
(inject_parent, notify_native); webhook keeps no early-out.

Adds two new tests: registry::all_kinds_registered (exhaustiveness
guard) and dispatch::unknown_kind_writes_error_run (unknown-kind path
through the new dispatcher).

External behaviour, wire protocol, DB schema, and existing tests
unchanged.
EOF
)"
```

---

## Task 5: CLAUDE.md update

**Files:**
- Modify: `CLAUDE.md` (root)

- [ ] **Step 1: Read the current "What this is" section**

```bash
grep -A 20 "^## What this is" /Users/i_strelov/Projects/roy/CLAUDE.md
```

Expected: a two-paragraph block describing `crates/roy` and `crates/roy-cli`.

- [ ] **Step 2: Add the two missing crates**

In `CLAUDE.md`, the "What this is" section currently lists `roy` and `roy-cli`. After the existing `roy-cli` bullet, add two more bullets:

```markdown
- **`crates/roy-scheduler`** — cron + one-shot fire dispatcher. Talks to the daemon over its Unix socket using `ClientCommand::Fire`; never reaches into `SessionManager`, `Engine`, or `Journal`. Owns its own SQLite state (`~/.local/state/roy-scheduler/state.db`) for triggers, fires, and subscribers.
- **`crates/roy-gateway`** — chat-platform → daemon bridge (v1: Telegram). Same boundary rule as `roy-scheduler`. Persists `chat_id → roy session_id` in a JSON file so chats survive restarts.
```

- [ ] **Step 3: Add the cross-crate boundary rule**

Immediately after the four bulleted crates, add one short paragraph:

```markdown
External crates (`roy-scheduler`, `roy-gateway`) depend on `roy` only for the wire-protocol types (`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`, `ErrorCode`, `StopReason`) and the `PidLock` utility. No direct calls into `SessionManager`, `SessionEngine`, `Journal`, or `Transport` are allowed — the Unix socket is the only API.
```

- [ ] **Step 4: Sanity-check that all four crates are now mentioned**

```bash
grep -c "crates/roy" /Users/i_strelov/Projects/roy/CLAUDE.md
```

Expected: at least 4 (one per crate in the "What this is" block, possibly more elsewhere in the doc).

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(claude): describe all 4 crates and their boundaries

Previously CLAUDE.md only described roy and roy-cli. Add roy-scheduler
and roy-gateway, plus the cross-crate boundary rule (external crates
talk to roy only through the wire-protocol types and the Unix socket).
EOF
)"
```

---

## Final verification

- [ ] **Step 1: Run the full CI gate locally**

```bash
cd /Users/i_strelov/Projects/roy
cargo fmt --all -- --check && \
cargo build --workspace --all-targets && \
cargo test --workspace --no-fail-fast
```

Expected: all three commands succeed. `cargo test` requires `python3` on PATH for the fake-acp-agent integration tests.

- [ ] **Step 2: Confirm the five commits exist and are well-shaped**

```bash
git log --oneline refactor/scheduler-design-cleanup ^master | head -10
```

Expected: five new commits on top of the spec commit (six total since master).

- [ ] **Step 3: Confirm done-definition from the spec**

Run each check from the spec's "Done definition" section:

```bash
# pid_lock.rs in roy-scheduler is gone
test ! -f /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/pid_lock.rs && echo OK

# ChainAgent does not appear anywhere
! grep -rn "ChainAgent\|chain_agent" /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/ && echo OK

# String-literal kind comparisons gone (apart from the enum's own parse/as_db)
! grep -rn 'kind == "oneshot"' /Users/i_strelov/Projects/roy/crates/roy-scheduler/src/ && echo OK

# CLAUDE.md mentions all four crates
grep -c "crates/roy" /Users/i_strelov/Projects/roy/CLAUDE.md
```

Expected: three `OK` lines and a count ≥ 4 from `grep -c`.

- [ ] **Step 4: Open the PR**

The branch is ready for review. PR title: `refactor(scheduler): design cleanup — Subscriber trait + 4 mechanical fixes`. PR body links to the spec and lists the five commits.
