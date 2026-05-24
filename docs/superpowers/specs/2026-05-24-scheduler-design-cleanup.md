# Scheduler design cleanup — design

Status: draft, awaiting user review.
Date: 2026-05-24.
Author: brainstorm with `strelov1`.

## Goal

Tighten the abstractions inside `roy-scheduler` (and one cross-crate
duplicate in `roy`) before they accrete more weight. The audit in the
preceding conversation turned up five concrete issues; this spec
captures the agreed fixes as a single PR.

The intent is **not** a sweeping rewrite — each item is mechanical
except for the Subscriber trait, and the whole PR is constrained so
external behaviour (wire protocol, DB schema, CLI surface) stays bit-for-
bit identical. All existing tests must pass without assertion changes.

## Non-goals

- **No `daemon.rs` decomposition** (3022 LOC God-file). Separate future PR.
- **No `FireOutcome` dedup** between `roy-scheduler` and `roy-gateway`. Both
  types are local conveniences over the same three `ServerEvent`
  variants; keeping them per-crate is the right boundary.
- **No `default_db_path` / `default_socket_path` consolidation** between
  `driver.rs` and `main.rs`. They live in different layers (lib defaults
  vs CLI defaults) and serve different callers.
- **No `ClientCommand` decomposition** (24 variants). Premature.
- **No `run_fire_for_agent` extraction** (130 LOC). Stylistic, not design.
- **No DB schema migrations.** All changes are Rust-side only; existing
  `fire_subscribers.kind`, `triggers.kind` columns stay `TEXT`.
- **No new subscriber kinds.** Adding `slack`, `email`, etc. is out of
  scope — the trait merely makes future additions cheap.

## Scope (the five items)

1. **PidLock dedup.** Drop the `roy-scheduler::pid_lock` copy and
   reuse `roy::PidLock` which is already exported from `roy/src/lib.rs`.
2. **`TriggerKind` enum.** Replace `Trigger.kind: String` with an enum
   that mirrors the `FireStatus` / `SubscriberKind` pattern already in
   `types.rs`.
3. **Remove `SubscriberKind::ChainAgent`.** The variant is a permanent
   "v1 reserved / not_implemented in v1" stub — CLAUDE.md forbids that
   shape. Either implement (out of scope) or remove. Removing.
4. **`Subscriber` trait + registry.** Replace the per-kind `match` in
   `subscribers/mod.rs::dispatch` with `Box<dyn Subscriber>` resolved
   through a static `HashMap<SubscriberKind, SubscriberCtor>` registry.
   This is the only non-mechanical item.
5. **CLAUDE.md update.** Today CLAUDE.md describes two crates (`roy`,
   `roy-cli`); the repo has four (`roy`, `roy-cli`, `roy-scheduler`,
   `roy-gateway`). Add the two missing crates and document the cross-
   crate boundary rule (external crates depend on roy only via wire-
   protocol types).

## Delivery

One PR on a new branch `refactor/scheduler-design-cleanup` cut from
`feature/roy-scheduler-v1`. Five commits, each stand-alone, each green
under the CI gate (`cargo fmt --check && cargo build --workspace
--all-targets && cargo test --workspace`):

1. `chore(roy): expose PidLock; drop roy-scheduler copy`
2. `refactor(scheduler): TriggerKind enum, drop kind: String`
3. `refactor(scheduler): remove SubscriberKind::ChainAgent`
4. `refactor(scheduler): Subscriber trait + registry`
5. `docs(claude): describe all 4 crates and their boundaries`

Order chosen by the graph of edits, not by impact: each commit's diff
should be readable without forward references. Subscriber-trait sits
last among the code commits so it isn't immediately rewritten by the
ChainAgent removal.

## Commit-level design

### 1. PidLock dedup

`crates/roy/src/lib.rs:9,22` already declares `pub mod pid_lock` and
re-exports `PidLock`. The duplicate `crates/roy-scheduler/src/pid_lock.rs`
(147 LOC) is removed wholesale; `mod pid_lock;` in
`roy-scheduler/src/main.rs:14` is deleted and the only call site
(`PidLock::acquire`) imports `use roy::PidLock`.

`roy-scheduler/Cargo.toml` already depends on `roy` (for protocol types),
so no dependency line moves.

Diff shape: −147 LOC, no behavioural change.

### 2. `TriggerKind` enum

In `roy-scheduler/src/types.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind { Cron, Oneshot }

impl TriggerKind {
    pub fn as_db(self) -> &'static str { ... }
    pub fn parse(s: &str) -> Option<Self> { ... }
}
```

`Trigger.kind` stays a `String` field for `FromRow` simplicity, but
becomes private (`pub(crate)` if not used outside the type) and exposes:

```rust
impl Trigger {
    pub fn kind(&self) -> TriggerKind { /* parse, expect — DB is internal */ }
    pub fn is_oneshot(&self) -> bool { matches!(self.kind(), TriggerKind::Oneshot) }
    pub fn is_paused(&self) -> bool { self.paused != 0 }
}
```

The trade-off: introducing `sqlx::Type` on the enum would push schema
churn we don't need. Keeping the column `TEXT` + a typed getter delivers
the safety benefit (call sites stop comparing string literals) without
DB risk. If we ever migrate to a `CHECK` constraint or a typed column,
the getter is the migration boundary.

All call sites that currently check `trig.kind == "oneshot"` move to
`trig.is_oneshot()`. Greppable: there's exactly one such site outside
`is_oneshot` itself (in `plan.rs`).

### 3. Remove `SubscriberKind::ChainAgent`

Mechanical:
- Drop the variant from `enum SubscriberKind` in `types.rs`.
- Drop the matching arms in `SubscriberKind::as_db` / `::parse`.
- Drop the dispatch arm in `subscribers/mod.rs:75-80`.
- Drop the variant from the iteration in `subscriber_kind_roundtrips`
  test.
- CLI in `roy-scheduler/src/main.rs:478` already validates `--kind`
  through `SubscriberKind::parse(&str) -> Option<Self>`. Removing the
  variant means `--kind chain_agent` now fails at insert time with
  "unknown subscriber kind", which is the desired behaviour.

**Existing DB rows.** Any row that still has `kind = "chain_agent"` in
`fire_subscribers` will be dispatched into the existing fall-through
in `dispatch` (`None` from `SubscriberKind::parse`) and produce a
`fire_subscriber_runs` row with `status = "error"` and message
`"unknown kind: chain_agent"`. This is strictly better than the current
hardcoded `"chain_agent: not_implemented in v1"` — fewer dead
identifiers in the codebase. No data migration needed.

### 4. `Subscriber` trait + registry

New layout under `crates/roy-scheduler/src/subscribers/`:

```
subscribers/
  mod.rs            — trait Subscriber, FireCtx, Outcome, RunStatus; re-exports
  dispatch.rs       — pub async fn dispatch(...) — single match becomes lookup
  registry.rs       — static registry: SubscriberKind → ctor
  inject_parent.rs  — impl Subscriber
  webhook.rs        — impl Subscriber
  notify_native.rs  — impl Subscriber
```

Trait shape:

```rust
#[async_trait]
pub trait Subscriber: Send + Sync {
    async fn run(&self, ctx: &FireCtx<'_>) -> Outcome;
}

pub struct FireCtx<'a> {
    pub pool: &'a SqlitePool,        // inject_parent: needed for Send / Resume
    pub socket_path: &'a Path,       // inject_parent: needed to reach the daemon
    pub fire: &'a Fire,
    pub agent_name: &'a str,
    pub success: Option<&'a FireSuccess>,
    pub error_message: Option<&'a str>,
}

pub enum RunStatus { Ok, Error, Skipped }

pub struct Outcome {
    pub status: RunStatus,
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}
```

Per-kind config parsing is the factory's job, not `run`'s — config text
moves through it once per dispatch, not once per `run` call:

```rust
type SubscriberCtor = fn(config_json: &str) -> Result<Box<dyn Subscriber>>;

fn registry() -> &'static HashMap<SubscriberKind, SubscriberCtor> {
    static R: OnceLock<HashMap<SubscriberKind, SubscriberCtor>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert(SubscriberKind::InjectParent, inject_parent::build);
        m.insert(SubscriberKind::Webhook,      webhook::build);
        m.insert(SubscriberKind::NotifyNative, notify_native::build);
        m
    })
}
```

`dispatch.rs::dispatch` becomes a flat loop:

```rust
for sub_row in subs {
    let Some(kind) = SubscriberKind::parse(&sub_row.kind) else {
        write_run(pool, &fire.id, &sub_row, Outcome::error(format!("unknown kind: {}", sub_row.kind))).await?;
        continue;
    };
    let Some(ctor) = registry().get(&kind) else {
        write_run(pool, &fire.id, &sub_row, Outcome::error("kind not registered".into())).await?;
        continue;
    };
    let outcome = match ctor(&sub_row.config) {
        Ok(sub) => sub.run(&ctx).await,
        Err(e)  => Outcome::error(format!("config parse: {e:#}")),
    };
    write_run(pool, &fire.id, &sub_row, outcome).await?;
}
```

**Skip-on-non-success** semantics move into the implementation that
needs them. Today `dispatch` skips `inject_parent` and `notify_native`
when `success.is_none()`. After the refactor those checks live in
`inject_parent::Subscriber::run` and `notify_native::Subscriber::run`
respectively — they observe `ctx.success` and return
`Outcome::skipped("...")`. `Webhook::run` keeps no such early-out
because it intentionally fires on both success and failure.

Object safety: `Subscriber` has one async method returning a non-`Self`
type → object-safe with `async_trait`. The same crate already uses
`async_trait` (`use async_trait::async_trait;` in `transport/mod.rs`).

The registry is intentionally not pluggable from outside the crate
(`OnceLock<HashMap>` not `RwLock`). v1 has no plugin surface; if a
future use-case needs runtime registration, swap to `RwLock` — the
caller-facing API (`dispatch`) does not change.

### 5. CLAUDE.md update

Edit `## What this is` to mention all four crates and the boundary rule.
No new top-level sections. Sample sentence:

> **`crates/roy-scheduler`** — cron + one-shot fire dispatcher. Talks to
> the daemon over the Unix socket using `ClientCommand::Fire`; never
> reaches into `SessionManager`, `Engine`, or `Journal`. Owns its own
> SQLite state (`~/.local/state/roy-scheduler/state.db`).
>
> **`crates/roy-gateway`** — chat-platform → daemon bridge (v1:
> Telegram). Same boundary rule as `roy-scheduler`. Persists
> `chat_id → session_id` in a JSON file.

Plus one short paragraph: "External crates (`roy-scheduler`,
`roy-gateway`) depend on `roy` only for the wire-protocol types
(`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`, `ErrorCode`,
`StopReason`) and the `PidLock` utility. No direct calls into the
session manager, engine, journal, or transport are allowed."

## Testing

The CI gate (`cargo fmt --check && cargo build --workspace --all-targets
&& cargo test --workspace`) is the primary acceptance.

Existing tests that must continue to pass without assertion changes:
- `crates/roy-scheduler/src/subscribers/webhook.rs` (module tests)
- `crates/roy-scheduler/src/subscribers/notify_native.rs` (module tests)
- `crates/roy-scheduler/src/subscribers/inject_parent.rs` (module tests)
- `crates/roy-scheduler/src/driver.rs::fire_agent_ad_hoc_dispatches_subscribers`
- `crates/roy-scheduler/src/types.rs::subscriber_kind_roundtrips`
  (minus one iteration for `ChainAgent`)
- `crates/roy-scheduler/tests/e2e.rs` (whole-system check)

New tests added in commit 4:
- `subscribers/registry.rs::all_kinds_registered` — iterate every
  `SubscriberKind` variant and assert `registry().contains_key(&k)`.
  Compile-time exhaustiveness is not feasible (no `strum`); this
  closes the gap.
- `subscribers/dispatch.rs::unknown_kind_writes_error_run` — feed a
  row with `kind = "bogus"` and assert the fire_subscriber_run shows
  `status = "error"` with the expected message.

No new tests for commits 1, 2, 3, 5 — each is a mechanical rename or
removal whose behaviour is already covered.

## Risks

1. **DB rows with `kind = "chain_agent"`** in user installs become
   "unknown kind" errors. Documented in commit 3's message. Down-grade
   path: re-add the variant. Up-side: dead identifier removed,
   `fire_subscriber_runs` rows now name the actual problem.
2. **`#[async_trait]` overhead** on `Box<dyn Subscriber>` adds a vtable
   indirection and one heap allocation per dispatch. Subscribers run
   after an I/O-bound fire (hundreds of ms minimum); the overhead is
   immeasurable.
3. **Registry is not externally pluggable.** v1 doesn't need it; v2
   migration path is `RwLock<HashMap>` with no public-API change.
4. **`TriggerKind::parse` panics inside `Trigger::kind()`** if the DB
   ever holds an unknown value. Acceptable — only this crate writes the
   column, and CLI insertions go through typed enums. If we ever ship
   a v2 trigger kind to v1 readers, this becomes the upgrade-gate.

## Done definition

- All five commits land on `refactor/scheduler-design-cleanup`.
- CI gate passes on the branch HEAD.
- `cargo test --workspace` passes locally with `python3` on `PATH`.
- The two new tests (registry exhaustiveness, unknown-kind dispatch)
  exist and pass.
- `crates/roy-scheduler/src/pid_lock.rs` is deleted.
- `SubscriberKind::ChainAgent` does not appear in any `*.rs` file in
  the workspace.
- `Trigger.kind == "oneshot"` does not appear in any `*.rs` file
  (replaced by `Trigger::is_oneshot()`).
- CLAUDE.md mentions all four crates in `## What this is`.
