# roy-protocol Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the shared wire surface (control/event/journal-types/harness-wire-types/pid_lock + a new framing codec) into a tiny leaf crate `roy-protocol`, so spokes depend on it instead of the full `roy` core — making the hub-and-spoke boundary compiler-enforced and giving the wire codec a single home.

**Architecture:** `roy-protocol` is a sync, leaf crate (no tokio / no ACP SDK / no rusqlite). `roy` core depends on it and re-exports its types at the old paths, so core/CLI/examples/core-tests are untouched. The five spokes swap their `roy` dependency for `roy-protocol`. Then the newline-JSON framing and `$ROY_SOCKET` resolution are centralized in `roy_protocol::wire` and routed through everywhere, and `StopReason` is serialized via its canonical `as_wire()` at the six sites that currently use `{:?}`.

**Tech Stack:** Rust 2021, Cargo workspace, serde/serde_json, thiserror, tokio (core + spokes only), rustfmt (max_width 100).

**Spec:** `docs/superpowers/specs/2026-05-29-crate-boundaries-design.md`

**Git convention:** All work happens on a feature branch (repo default branch is `master`). Each task ends with a commit on that branch.

---

## File Structure

**New crate `crates/roy-protocol/`:**
- `Cargo.toml` — leaf crate manifest
- `src/lib.rs` — module declarations + top-level re-exports
- `src/error.rs` — `RoyError`, `Result` (moved whole from `roy`)
- `src/event.rs` — `TurnEvent`, `StopReason`, `event_to_json`, `event_from_json` (moved whole)
- `src/control.rs` — `ClientCommand`, `ServerEvent`, `ErrorCode`, `FireTarget`, `ConnectionSpec` (moved whole, with one import path edit)
- `src/journal.rs` — `Seq`, `JournalEntry`, `parse_entry_line` (subset split from `roy`)
- `src/harnesses.rs` — `Harness`, `HarnessInfo`, `ModelInfo`, `HarnessesConfigStatus` (subset split from `roy`)
- `src/pid_lock.rs` — `PidLock` + helpers (moved whole)
- `src/wire.rs` — **NEW** `encode_line`, `decode_line`, `default_socket_path`

**Modified in `crates/roy/`:**
- `Cargo.toml` — add `roy-protocol` dependency
- `src/lib.rs` — replace moved `pub mod`s with re-exports from `roy-protocol`
- `src/journal.rs` — keep `Journal`/`ArchivedJournal` actors + `unix_now_millis`; re-export the moved types
- `src/harnesses_config.rs` — keep the loader; re-export the moved wire types
- `src/daemon.rs` — route framing through `roy_protocol::wire`
- (`error.rs`, `event.rs`, `control.rs`, `pid_lock.rs` deleted — content lives in `roy-protocol`)

**Modified spokes (dependency + import swap, then codec/StopReason routing):**
- `crates/roy-mcp/`, `crates/roy-scheduler/`, `crates/roy-inbound/`, `crates/roy-gateway/`, `crates/roy-management/`, `crates/roy-cli/`

---

## Task 0: Create the feature branch

- [ ] **Step 1: Branch off master**

```bash
git checkout -b feat/roy-protocol-extraction
git status
```
Expected: `On branch feat/roy-protocol-extraction`, clean tree.

---

## Task 1: Scaffold the empty `roy-protocol` crate

**Files:**
- Create: `crates/roy-protocol/Cargo.toml`
- Create: `crates/roy-protocol/src/lib.rs`

The workspace uses `members = ["crates/*"]`, so no root `Cargo.toml` edit is needed.

- [ ] **Step 1: Write the manifest**

Read `crates/roy/Cargo.toml` first to copy the exact version strings used for `serde`, `serde_json`, `thiserror`, and the syscall crate `pid_lock` uses (grep `pid_lock.rs` for `libc`/`nix`/`rustix`; add only that one). Then create `crates/roy-protocol/Cargo.toml`:

```toml
[package]
name = "roy-protocol"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "<match roy>", features = ["derive"] }
serde_json = "<match roy>"
thiserror = "<match roy>"
# add the one syscall crate pid_lock.rs already uses, if any (e.g. libc)
```

- [ ] **Step 2: Write a placeholder lib.rs**

```rust
//! Wire-protocol surface shared by the roy daemon and every trigger.
//! Sync, leaf crate: no tokio, no ACP SDK, no rusqlite.
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p roy-protocol`
Expected: PASS (compiles an empty lib).

- [ ] **Step 4: Commit**

```bash
git add crates/roy-protocol
git commit -m "feat(roy-protocol): scaffold empty leaf crate"
```

---

## Task 2: Populate `roy-protocol` by copying the wire surface in

This task is **purely additive** — `roy` keeps its own copies for now, so the workspace still builds (two crates, no symbol conflict). Verification is that the new crate compiles and its moved unit tests pass.

**Files:**
- Create: `crates/roy-protocol/src/{error,event,control,journal,harnesses,pid_lock}.rs`
- Modify: `crates/roy-protocol/src/lib.rs`

- [ ] **Step 1: Copy whole modules**

Copy these files verbatim from `crates/roy/src/` into `crates/roy-protocol/src/` (same filename):
- `error.rs` (no edits)
- `event.rs` (no edits — its `use crate::error::{Result, RoyError}` resolves in the new crate)
- `pid_lock.rs` (no edits — uses `crate::error` + `std::fs`)
- `control.rs` — **one edit**: it imports `use crate::journal::{JournalEntry, Seq};` (keep) and references `crate::harnesses_config::HarnessInfo` / `crate::harnesses_config::HarnessesConfigStatus`. Change every `crate::harnesses_config::` to `crate::harnesses::` (occurs at the `ServerEvent::HarnessesList` variant fields and in the `#[cfg(test)]` module — grep `harnesses_config` in the copied file and replace with `harnesses`).

- [ ] **Step 2: Create `journal.rs` with the types subset only**

Create `crates/roy-protocol/src/journal.rs` with exactly these items copied from `roy/src/journal.rs`, and make `parse_entry_line` **public** (it was private):

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, RoyError};
use crate::event::{event_from_json, TurnEvent};

pub type Seq = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: Seq,
    /// Wall-clock millis since epoch. `seq` is still the ordering key — many
    /// events share a millisecond during a streamed turn.
    pub ts_ms: u64,
    pub event: TurnEvent,
}

/// Parse one JSONL line into a `JournalEntry`. Single source of truth for the
/// on-disk format, used by the `Journal`/`ArchivedJournal` actors in roy core.
/// Returns `Protocol` errors with the offending line so a corrupt journal
/// surfaces clearly instead of silently dropping entries.
pub fn parse_entry_line(line: &str) -> Result<JournalEntry> {
    let v: Value = serde_json::from_str(line).map_err(|e| RoyError::Protocol(e.to_string()))?;
    let seq = v
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| RoyError::Protocol(format!("journal entry missing seq: {line}")))?;
    let ts_ms = v
        .get("ts_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| RoyError::Protocol(format!("journal entry missing ts_ms: {line}")))?;
    let event = event_from_json(
        v.get("event")
            .ok_or_else(|| RoyError::Protocol(format!("journal entry missing event: {line}")))?,
    )?;
    Ok(JournalEntry { seq, ts_ms, event })
}
```

Do **not** copy `unix_now_millis`, `Journal`, `JournalInner`, or `ArchivedJournal` — those stay in core (Task 3).

- [ ] **Step 3: Create `harnesses.rs` with the wire types subset only**

Create `crates/roy-protocol/src/harnesses.rs` with exactly these items copied from `roy/src/harnesses_config.rs` (lines defining them): the `Harness` enum **plus its `impl Harness` (ALL/as_str), `impl Display`, `impl FromStr`** (all pure), `ModelInfo`, `HarnessInfo`, `HarnessesConfigStatus`. Preserve their derives and serde attributes exactly. Add the imports they need at the top:

```rust
use serde::{Deserialize, Serialize};
```

Do **not** copy `HarnessesConfig`, `HarnessEntry`, `ModelEntry`, `HarnessesConfigError`, `LoadOutcome`, `config_path`, `load_or_bootstrap`, `write_sample`, `into_wire`, or `SAMPLE_TOML` — the loader stays in core.

- [ ] **Step 4: Write `roy-protocol/src/lib.rs`**

```rust
//! Wire-protocol surface shared by the roy daemon and every trigger.
//! Sync, leaf crate: no tokio, no ACP SDK, no rusqlite.

pub mod control;
pub mod error;
pub mod event;
pub mod harnesses;
pub mod journal;
pub mod pid_lock;
pub mod wire;

pub use control::{ClientCommand, ConnectionSpec, ErrorCode, FireTarget, ServerEvent};
pub use error::{Result, RoyError};
pub use event::{event_from_json, event_to_json, StopReason, TurnEvent};
pub use harnesses::{Harness, HarnessInfo, HarnessesConfigStatus, ModelInfo};
pub use journal::{parse_entry_line, JournalEntry, Seq};
pub use pid_lock::{peek_pid, pid_alive, pid_path_for_socket, PidLock};
```

The `wire` module does not exist yet — it is added in Task 9. The `lib.rs` above intentionally has **no** `pub mod wire;` / `wire::*` re-export; Task 9 adds both.

- [ ] **Step 5: Build and run the moved tests**

Run: `cargo build -p roy-protocol`
Expected: PASS.

Run: `cargo test -p roy-protocol`
Expected: PASS — the `#[cfg(test)]` modules copied inside `event.rs`, `control.rs`, `pid_lock.rs` run against the new crate.

- [ ] **Step 6: Verify the workspace still builds (roy still has its own copies)**

Run: `cargo build --workspace`
Expected: PASS — `roy` is unchanged and unrelated to the new crate yet.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-protocol
git commit -m "feat(roy-protocol): populate wire surface (control/event/journal-types/harness-wire-types/pid_lock)"
```

---

## Task 3: Swap `roy` core to depend on `roy-protocol`

Remove the now-duplicated definitions from `roy`, add the dependency, and re-export from `roy-protocol` at the old paths so core/CLI/examples/core-tests are untouched.

**Files:**
- Modify: `crates/roy/Cargo.toml`
- Delete: `crates/roy/src/error.rs`, `crates/roy/src/event.rs`, `crates/roy/src/control.rs`, `crates/roy/src/pid_lock.rs`
- Modify: `crates/roy/src/lib.rs`
- Modify: `crates/roy/src/journal.rs` (keep actors, re-export types)
- Modify: `crates/roy/src/harnesses_config.rs` (keep loader, re-export wire types)

- [ ] **Step 1: Add the dependency**

In `crates/roy/Cargo.toml` `[dependencies]`, add:
```toml
roy-protocol = { path = "../roy-protocol" }
```

- [ ] **Step 2: Delete the moved-whole modules**

```bash
git rm crates/roy/src/error.rs crates/roy/src/event.rs crates/roy/src/control.rs crates/roy/src/pid_lock.rs
```

- [ ] **Step 3: Convert `journal.rs` to actor + re-export**

At the top of `crates/roy/src/journal.rs`, replace the type/`parse_entry_line` definitions you deleted with a re-export, and keep the actors. The head of the file becomes:

```rust
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
// ...keep the file's existing tokio/std imports...

pub use roy_protocol::journal::{parse_entry_line, JournalEntry, Seq};

fn unix_now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Journal, JournalInner, impl Journal, ArchivedJournal, impl ArchivedJournal — UNCHANGED
```

Inside the actor code, calls to `parse_entry_line(...)` now resolve to the re-exported fn. References to `RoyError`/`Result` resolve via `crate::error` (see Step 5). No actor-body edits needed beyond imports.

- [ ] **Step 4: Convert `harnesses_config.rs` to loader + re-export**

At the top of `crates/roy/src/harnesses_config.rs`, add a re-export of the moved wire types and delete their definitions (the `Harness` enum + impls, `ModelInfo`, `HarnessInfo`, `HarnessesConfigStatus`). Keep `HarnessesConfig`, `HarnessEntry`, `ModelEntry`, `HarnessesConfigError`, `LoadOutcome`, `config_path`, `load_or_bootstrap`, `write_sample`, `into_wire`, `SAMPLE_TOML`. Add near the top:

```rust
pub use roy_protocol::harnesses::{Harness, HarnessInfo, HarnessesConfigStatus, ModelInfo};
```

The loader body (`into_wire`, etc.) keeps referencing `HarnessInfo`/`ModelInfo`/`Harness` — now resolved via the re-export. No body edits needed.

- [ ] **Step 5: Rewrite `roy/src/lib.rs`**

```rust
pub mod daemon;
pub mod engine;
pub mod harnesses_config;
pub mod journal;
pub mod manager;
pub mod session_store;
pub mod transport;

// Wire surface lives in roy-protocol; re-export at the historical paths so
// roy-cli, examples, and core tests keep using `roy::...` unchanged.
// NOTE: `wire` is intentionally absent here — Task 9 adds it once the module exists.
pub use roy_protocol::{control, error, event, pid_lock};

pub use roy_protocol::control::{ClientCommand, ConnectionSpec, ErrorCode, FireTarget, ServerEvent};
pub use roy_protocol::error::{Result, RoyError};
pub use roy_protocol::event::{event_from_json, event_to_json, StopReason, TurnEvent};
pub use roy_protocol::pid_lock::{peek_pid, pid_alive, PidLock};

// These come back out through the wrapper modules (types from roy-protocol,
// actor/loader from core).
pub use harnesses_config::{Harness, HarnessInfo, HarnessesConfigStatus, ModelInfo};
pub use journal::{ArchivedJournal, Journal, JournalEntry, Seq};

pub use daemon::{Daemon, DefaultTransportFactory, ServeOpts, TransportFactory};
pub use engine::{Attach, EngineOpts, InputLease, SessionEngine, SessionSpawnConfig};
pub use manager::SessionManager;
pub use transport::{AcpConfig, AcpTransport, Handle, PermissionPolicy, Transport};
```

- [ ] **Step 6: Fix internal `crate::` paths if the compiler complains**

`crate::control::X`, `crate::event::X`, `crate::error::X`, `crate::pid_lock::X` resolve through the Step-5 re-exports. If any core module used `crate::harnesses_config::Harness` it still resolves (wrapper re-export). Build and fix any stragglers the compiler names:

Run: `cargo build -p roy`
Expected: PASS. Fix any unresolved-import errors by pointing them at the re-exported path the compiler suggests.

- [ ] **Step 7: Run core tests**

Run: `cargo test -p roy`
Expected: PASS — daemon unit tests, journal tests, etc. unchanged.

- [ ] **Step 8: Full workspace build (spokes still on `roy`, which still exports everything)**

Run: `cargo build --workspace`
Expected: PASS — spokes import `roy::ClientCommand` etc., still satisfied by the re-exports.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(roy): depend on roy-protocol; re-export wire types at historical paths"
```

---

## Tasks 4–8: Migrate spokes off `roy` onto `roy-protocol`

Each spoke task is the same shape: swap the dependency, swap the imports, build + test that crate. After each, the spoke can no longer reach core internals. `roy-cli` is intentionally NOT migrated (it needs `Daemon`).

### Task 4: Migrate `roy-mcp`

**Files:**
- Modify: `crates/roy-mcp/Cargo.toml`
- Modify: `crates/roy-mcp/src/lib.rs`, `crates/roy-mcp/src/serve_connections/spec.rs`

- [ ] **Step 1: Swap the dependency**

In `crates/roy-mcp/Cargo.toml`, replace `roy = { path = "../roy" }` with `roy-protocol = { path = "../roy-protocol" }`.

- [ ] **Step 2: Swap imports**

Replace every `roy::` with `roy_protocol::` in the crate. Known sites (verify with `grep -rn 'roy::' crates/roy-mcp/src`):
- `src/lib.rs:16` `use roy::{ClientCommand, Harness, ServerEvent, TurnEvent};` → `use roy_protocol::{...}`
- `src/lib.rs:431` `use roy::FireTarget;` → `use roy_protocol::FireTarget;`
- `src/serve_connections/spec.rs:9` `pub use roy::ConnectionSpec;` → `pub use roy_protocol::ConnectionSpec;`

- [ ] **Step 3: Build + test**

Run: `cargo build -p roy-mcp && cargo test -p roy-mcp`
Expected: PASS. (If a symbol is missing from `roy-protocol`, it means a non-wire import slipped through — STOP and reassess; the spec asserts none exist.)

- [ ] **Step 4: Commit**

```bash
git add crates/roy-mcp
git commit -m "refactor(roy-mcp): depend on roy-protocol instead of roy core"
```

### Task 5: Migrate `roy-scheduler`

**Files:**
- Modify: `crates/roy-scheduler/Cargo.toml`
- Modify: `crates/roy-scheduler/src/roy_client.rs`, `crates/roy-scheduler/src/driver.rs` (test `use roy::` sites)

- [ ] **Step 1: Swap dependency** — `roy` → `roy-protocol` in `Cargo.toml`.
- [ ] **Step 2: Swap imports** — `grep -rn 'roy::' crates/roy-scheduler/src` and change each `roy::` to `roy_protocol::`. Known: `roy_client.rs:11` `use roy::{ClientCommand, FireTarget, ServerEvent, TurnEvent};` and the `#[cfg(test)]` `use roy::{...}` blocks in `driver.rs` (lines ~582, 664, 709, 778).
- [ ] **Step 3: Build + test** — Run: `cargo build -p roy-scheduler && cargo test -p roy-scheduler` — Expected: PASS.
- [ ] **Step 4: Commit** — `git add crates/roy-scheduler && git commit -m "refactor(roy-scheduler): depend on roy-protocol instead of roy core"`

### Task 6: Migrate `roy-inbound`

**Files:**
- Modify: `crates/roy-inbound/Cargo.toml`
- Modify: `src/session.rs`, `src/reply.rs`, `src/daemon_client.rs`, `src/channels/webhook/reply.rs`, `tests/integration.rs`

- [ ] **Step 1: Swap dependency** — `roy` → `roy-protocol`.
- [ ] **Step 2: Swap imports** — `grep -rn 'roy::' crates/roy-inbound` and change each `roy::` → `roy_protocol::`. Known: `session.rs:68` `roy::FireTarget`; `reply.rs:9-10` `roy::event::TurnEvent` + `roy::ErrorCode`; `daemon_client.rs:10` `use roy::{ClientCommand, FireTarget, ServerEvent, TurnEvent};` and `:127` test `use roy::{ErrorCode, StopReason};`; `channels/webhook/reply.rs:8` `roy::event::TurnEvent`; `tests/integration.rs:8` `use roy::{ServerEvent, StopReason, TurnEvent};`.
- [ ] **Step 3: Build + test** — Run: `cargo build -p roy-inbound && cargo test -p roy-inbound` — Expected: PASS.
- [ ] **Step 4: Commit** — `git add crates/roy-inbound && git commit -m "refactor(roy-inbound): depend on roy-protocol instead of roy core"`

### Task 7: Migrate `roy-gateway`

**Files:**
- Modify: `crates/roy-gateway/Cargo.toml`
- Modify: `src/daemon.rs`, `src/orchestrator.rs`, `src/formatting.rs` (`roy-auth` dependency stays untouched)

- [ ] **Step 1: Swap dependency** — replace `roy = { path = "../roy" }` with `roy-protocol = { path = "../roy-protocol" }`. Leave the `roy-auth` line.
- [ ] **Step 2: Swap imports** — `grep -rn 'roy::' crates/roy-gateway` (careful NOT to touch `roy_auth::`). Change each `roy::` → `roy_protocol::`. Known: `daemon.rs:9` `use roy::control::{ClientCommand, ServerEvent};`, `:10` `roy::event::TurnEvent`, `:11` `roy::journal::JournalEntry`, test `:219` `roy::event::{StopReason, TurnEvent}`, `:222` `roy::journal::JournalEntry as JE`; `orchestrator.rs:10` `roy::event::TurnEvent`, `:199` `roy::event::StopReason`; `formatting.rs:6` + `:152` `roy::event::{...}`.
- [ ] **Step 3: Build + test** — Run: `cargo build -p roy-gateway && cargo test -p roy-gateway` — Expected: PASS.
- [ ] **Step 4: Commit** — `git add crates/roy-gateway && git commit -m "refactor(roy-gateway): depend on roy-protocol instead of roy core"`

### Task 8: Migrate `roy-management`

**Note:** `roy-management` also depends on `roy-scheduler` and `roy-auth` — leave those untouched (they are separate audit findings). Only swap the `roy` core dependency.

**Files:**
- Modify: `crates/roy-management/Cargo.toml`
- Modify: `src/roy_client.rs` (and any other `roy::` site)

- [ ] **Step 1: Swap dependency** — replace `roy = { path = "../roy" }` with `roy-protocol = { path = "../roy-protocol" }`. Leave `roy-auth` and `roy-scheduler` lines.
- [ ] **Step 2: Swap imports** — `grep -rn '\broy::' crates/roy-management` (NOT `roy_auth::`/`roy_scheduler::`). Known: `roy_client.rs:10` `use roy::{ClientCommand, ServerEvent};`.
- [ ] **Step 3: Build + test** — Run: `cargo build -p roy-management && cargo test -p roy-management` — Expected: PASS.
- [ ] **Step 4: Verify the boundary is now compiler-enforced**

Run: `grep -rn '\broy::' crates/roy-mcp crates/roy-scheduler crates/roy-inbound crates/roy-gateway crates/roy-management --include='*.rs'`
Expected: **no matches** (every spoke now uses `roy_protocol::`). This is the structural win: spokes cannot name core internals.

- [ ] **Step 5: Commit** — `git add crates/roy-management && git commit -m "refactor(roy-management): depend on roy-protocol instead of roy core"`

---

## Task 9: Add the `wire` framing codec to `roy-protocol` (TDD)

**Files:**
- Create: `crates/roy-protocol/src/wire.rs`
- Modify: `crates/roy-protocol/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/roy-protocol/src/wire.rs`:

```rust
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{Result, RoyError};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ClientCommand;

    #[test]
    fn encode_then_decode_roundtrips() {
        let cmd = ClientCommand::List;
        let frame = encode_line(&cmd).unwrap();
        assert!(frame.ends_with(b"\n"), "frame must be newline-terminated");
        let text = String::from_utf8(frame).unwrap();
        let back: ClientCommand = decode_line(&text).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn decode_line_tolerates_trailing_newline_and_spaces() {
        // The bug this codec exists to kill: some call sites trimmed, one did not.
        let cmd = ClientCommand::List;
        let json = serde_json::to_string(&cmd).unwrap();
        let back: ClientCommand = decode_line(&format!("  {json}\n")).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn decode_line_surfaces_garbage_as_protocol_error() {
        let err = decode_line::<ClientCommand>("{not json").unwrap_err();
        assert!(matches!(err, RoyError::Protocol(_)));
    }
}
```

This requires `ClientCommand: PartialEq`. Check whether it derives `PartialEq`; if not, either pick a `ClientCommand` variant comparison via serde round-trip on the JSON string instead (compare `serde_json::to_string(&back) == serde_json::to_string(&cmd)`), to avoid adding a derive purely for tests.

- [ ] **Step 2: Run the tests — expect failure**

Run: `cargo test -p roy-protocol wire::`
Expected: FAIL — `encode_line`/`decode_line` not defined.

- [ ] **Step 3: Implement the codec + socket path**

Add above the `#[cfg(test)]` block in `wire.rs`:

```rust
/// Serialize a value to one newline-terminated JSON frame. Single source of
/// truth for the daemon's line framing — pair with [`decode_line`].
pub fn encode_line<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut buf = serde_json::to_vec(value).map_err(|e| RoyError::Protocol(e.to_string()))?;
    buf.push(b'\n');
    Ok(buf)
}

/// Parse one framed line into a value. Trims surrounding whitespace/newline
/// first, so callers need not remember to `.trim()` — the exact divergence
/// (`roy-gateway` did not trim) this helper exists to remove.
pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<T> {
    serde_json::from_str(line.trim()).map_err(|e| RoyError::Protocol(e.to_string()))
}

/// Resolve the daemon Unix-socket path: `$ROY_SOCKET` if set, else
/// `$HOME/.roy/daemon.sock`. Single source of truth — replaces six
/// byte-identical copies across the CLI and spokes.
pub fn default_socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}
```

- [ ] **Step 4: Export it**

In `crates/roy-protocol/src/lib.rs`, add `pub mod wire;` (alphabetical, after `pid_lock`) and:
```rust
pub use wire::{decode_line, default_socket_path, encode_line};
```

- [ ] **Step 5: Run the tests — expect pass**

Run: `cargo test -p roy-protocol wire::`
Expected: PASS (3 tests).

- [ ] **Step 6: Re-add `wire` to roy core re-export**

In `crates/roy/src/lib.rs`, change `pub use roy_protocol::{control, error, event, pid_lock};` to include `wire`:
```rust
pub use roy_protocol::{control, error, event, pid_lock, wire};
```

Run: `cargo build -p roy`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-protocol crates/roy/src/lib.rs
git commit -m "feat(roy-protocol): add wire framing codec + default_socket_path"
```

---

## Task 10: Route the daemon, CLI, and spoke clients through `wire`

Replace the hand-rolled framing and socket-path code with calls to `roy_protocol::wire`. Behavior is identical; the point is a single source of truth.

**Files:**
- Modify: `crates/roy/src/daemon.rs` (server framing)
- Modify: `crates/roy-cli/src/main.rs` (socket path + client framing)
- Modify: `crates/roy-mcp/src/lib.rs`, `crates/roy-scheduler/src/{cli.rs,driver.rs,roy_client.rs}`, `crates/roy-inbound/src/{cli.rs,daemon_client.rs}`, `crates/roy-gateway/src/{lib.rs,daemon.rs}`, `crates/roy-management/src/{lib.rs,roy_client.rs}`

- [ ] **Step 1: Daemon writer — use `encode_line`**

In `crates/roy/src/daemon.rs` `line_writer_loop`, replace the body of the `while let Some(event)` loop:

```rust
    while let Some(event) = rx.recv().await {
        let frame = match roy_protocol::wire::encode_line(&event) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if writer.write_all(&frame).await.is_err() {
            break;
        }
        if writer.flush().await.is_err() {
            break;
        }
    }
```

- [ ] **Step 2: Daemon reader — use `decode_line`**

In `dispatch_one_command`, replace:
```rust
        let cmd: ClientCommand = match serde_json::from_str(text) {
```
with:
```rust
        let cmd: ClientCommand = match roy_protocol::wire::decode_line(text) {
```
(`text` is pre-trimmed by `dispatch_lines`; `decode_line` trims again harmlessly. Leave the empty-line skip in `dispatch_lines` as-is.)

- [ ] **Step 3: Build + test core**

Run: `cargo build -p roy && cargo test -p roy`
Expected: PASS (daemon socket tests exercise the new framing).

- [ ] **Step 4: CLI — socket path + client framing**

In `crates/roy-cli/src/main.rs`:
- Delete the local `fn default_socket() -> PathBuf { ... }` and replace its call sites with `roy::wire::default_socket_path()`. (`grep -n 'default_socket()' crates/roy-cli/src/main.rs`.)
- In the send path (`send_cmd`, ~570-576) replace the `to_string + write_all + write_all(b"\n") + flush` sequence with:
  ```rust
  writer.write_all(&roy::wire::encode_line(&cmd)?).await?;
  writer.flush().await?;
  ```
- In the read path (`read_event`, ~1226-1234) replace `serde_json::from_str(line.trim())` with `roy::wire::decode_line(&line)`. Keep the crate's existing unknown-event policy (it rejects) — only the parse mechanics change.

Run: `cargo build -p roy-cli && cargo test -p roy-cli`
Expected: PASS.

- [ ] **Step 5: Spoke clients — socket path + framing**

For each spoke, replace the local `$ROY_SOCKET → ~/.roy/daemon.sock` resolver with `roy_protocol::wire::default_socket_path()`, and the inline framing with `encode_line`/`decode_line`. Sites (from the audit):
- `roy-scheduler`: `cli.rs:247-253` and `driver.rs:110-116` (socket path); `roy_client.rs:57-68` (framing).
- `roy-inbound`: `cli.rs:120-125` (socket path); `daemon_client.rs:49-60` (framing).
- `roy-management`: `lib.rs:131-136` (socket path); `roy_client.rs:150-163` + `:67-79` (framing).
- `roy-gateway`: `lib.rs:79-84` (socket path); `daemon.rs:89-102` `TurnConn` (framing) — **note** `daemon.rs:101` is the no-trim site; `decode_line` fixes it. Do **not** touch `ws.rs` (transparent byte relay).
- `roy-mcp`: `lib.rs:262-288` + `:322-330` (framing). MCP resolves its socket path however it does today; if it has its own resolver, route it to `default_socket_path()`.

Pattern for each framing write:
```rust
writer.write_all(&roy_protocol::wire::encode_line(&cmd)?).await?;
writer.flush().await?;
```
Pattern for each framing read (the spoke keeps its own "hung up" / unknown-event handling; only the parse changes):
```rust
let evt: ServerEvent = roy_protocol::wire::decode_line(&raw)?;
```

- [ ] **Step 6: Build + test each migrated spoke**

Run: `cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 7: Verify the socket-path duplication is gone**

Run: `grep -rn '.roy/daemon.sock' crates --include='*.rs'`
Expected: a single match inside `crates/roy-protocol/src/wire.rs`.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: route daemon + CLI + spoke clients through roy_protocol::wire"
```

---

## Task 11: Serialize `StopReason` via `as_wire()` at the six `{:?}` sites (audit rank 2, TDD)

**Files:**
- Modify: `crates/roy-cli/src/main.rs` (~1005, ~1077), `crates/roy-mcp/src/lib.rs` (~414, ~505), `crates/roy-inbound/src/daemon_client.rs` (~82), `crates/roy-scheduler/src/roy_client.rs` (~87)
- Modify: `crates/roy-inbound` test that hard-codes `"EndTurn"`
- Test: add a regression assertion in `roy-scheduler` (persisted `fires.stop_reason`) or `roy-inbound` (webhook body)

- [ ] **Step 1: Write the failing regression test**

In `crates/roy-scheduler`, add a test that drives a `FireOutcome::Done` through `roy_client` (or the nearest seam that produces `FireSuccess.stop_reason`) with `StopReason::EndTurn` and asserts the string is `"end_turn"`, not `"EndTurn"`. If the existing test harness in `roy_client.rs`/`driver.rs` makes that awkward, add a focused unit test on the conversion site. Example shape:

```rust
#[test]
fn fire_success_stop_reason_is_snake_case() {
    // Construct the StopReason → string exactly as fire() does:
    let sr = roy_protocol::StopReason::EndTurn;
    assert_eq!(sr.as_wire(), "end_turn");
    // and assert the producing code path uses as_wire (see Step 3 sites).
}
```

- [ ] **Step 2: Run — expect the buggy sites still emit `{:?}`**

Grep to confirm the buggy sites exist: `grep -rn 'format!("{stop_reason:?}")' crates` — expect 6 in-scope matches (plus 1 in `roy-gateway/orchestrator.rs:179` that is OUT of scope).

- [ ] **Step 3: Replace the six in-scope sites**

At each of these, change `format!("{stop_reason:?}")` → `stop_reason.as_wire().to_string()`:
- `crates/roy-cli/src/main.rs` ~1005, ~1077
- `crates/roy-mcp/src/lib.rs` ~414, ~505
- `crates/roy-inbound/src/daemon_client.rs` ~82
- `crates/roy-scheduler/src/roy_client.rs` ~87

**Do NOT touch** `crates/roy-gateway/src/orchestrator.rs:179` — it feeds a human-facing Telegram error footer.

- [ ] **Step 4: Fix the inbound test that hard-codes the bug**

Find it: `grep -rn 'EndTurn' crates/roy-inbound`. Change the expected value from `"EndTurn"` to `"end_turn"`.

- [ ] **Step 5: Run — expect pass**

Run: `cargo test -p roy-scheduler -p roy-inbound -p roy-mcp -p roy-cli`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix: serialize StopReason via as_wire() across spokes (snake_case wire vocabulary)"
```

---

## Task 12: Documentation touch-ups

**Files:**
- Modify: `CLAUDE.md`, `docs/architecture.md`, `crates/roy-protocol/src/control.rs` (header comment), `README.md`

- [ ] **Step 1: CLAUDE.md**

- Add `roy-protocol` to the crate list (the leaf wire crate the spokes depend on).
- Update the paragraph beginning "External crates (`roy-mcp`, ...) depend on `roy` only for the wire-protocol types": the boundary is now "depend on `roy-protocol` (not `roy` core); the allowed surface is whatever `roy-protocol` exports", and note this is compiler-enforced. Remove the now-redundant hand-maintained type list.

- [ ] **Step 2: control.rs header comment**

If the moved `crates/roy-protocol/src/control.rs` header says framing is "length-prefixed", correct it to "newline-delimited JSON (`roy_protocol::wire`)". (`grep -n 'length-prefixed' crates/roy-protocol/src/control.rs`.)

- [ ] **Step 3: docs/architecture.md**

In the crate-layering section, add `roy-protocol` as the leaf and note spokes depend on it, not on `roy` core.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md docs/architecture.md crates/roy-protocol/src/control.rs README.md
git commit -m "docs: document roy-protocol leaf crate + compiler-enforced spoke boundary"
```

---

## Task 13: Final full-workspace gate

- [ ] **Step 1: Run the exact CI gate locally**

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```
Expected: all three PASS. (Integration tests spawn `python3 tests/scripts/fake-acp-agent.py` — ensure `python3` is on PATH.)

- [ ] **Step 2: Confirm the structural invariants**

```bash
# spokes name only roy_protocol, never roy core:
grep -rn '\broy::' crates/roy-mcp crates/roy-scheduler crates/roy-inbound crates/roy-gateway crates/roy-management --include='*.rs'   # expect: no matches
# socket path single-sourced:
grep -rn '.roy/daemon.sock' crates --include='*.rs'                                                                                  # expect: 1 match (wire.rs)
# StopReason {:?} only in the human-facing gateway footer:
grep -rn 'format!("{stop_reason:?}")' crates                                                                                          # expect: 1 match (gateway orchestrator.rs)
```

- [ ] **Step 3: Final commit if fmt changed anything**

```bash
git add -A && git commit -m "style: cargo fmt" || true
```

---

## Self-Review notes (for the implementer)

- **If a spoke fails to build after the dependency swap with a missing symbol**, it means a non-wire import slipped past the audit. STOP — that symbol is either (a) genuinely a wire type that belongs in `roy-protocol` (add it there), or (b) a real boundary violation that this refactor just surfaced (flag it; do not paper over by re-adding the `roy` core dependency).
- **`ClientCommand`/`ServerEvent` `PartialEq`**: Task 9 tests assume it; if the derive is absent, compare serialized JSON strings instead of adding a derive solely for tests.
- **Out of scope (do NOT do here):** audit ranks 3 & 4 (`management → scheduler` private DB; `roy-auth` SELECT on `projects`), daemon god-object split, TurnEvent normalization quality. Separate plans.
