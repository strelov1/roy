# Extract `roy-protocol`: make the hub-and-spoke boundary compiler-enforced

**Date:** 2026-05-29
**Status:** Approved (design), pending implementation plan
**Origin:** abstraction-boundary audit (2026-05) — see `project_roy_abstraction_audit` memory.

## Problem

The workspace is hub-and-spoke: `roy` core is the hub; `roy-mcp`, `roy-scheduler`,
`roy-gateway`, `roy-management`, `roy-inbound` are spokes that must reach the daemon
**only** through its Unix socket, depending on `roy` solely for wire-protocol types.

The audit confirmed this invariant **holds today** — no spoke calls into
`SessionManager`/`SessionEngine`/`Journal`(actor)/`Transport`/`Daemon`/`SessionStore`.
But it holds *by convention*: the compiler does not enforce it, because every spoke
depends on the full `roy` crate (ACP SDK, daemon, `rusqlite`, the session engine,
tokio-heavy core) just to obtain a handful of wire types. Two consequences:

1. **Nothing prevents a future regression** — a spoke *can* write `roy::SessionManager`;
   only review and a (already-stale) prose list in `CLAUDE.md` stops it.
2. **The wire codec has no home.** `control.rs` ships wire *types* but no wire *codec*,
   so the newline-JSON `connect → frame → decode` dance is hand-reimplemented 5–6×
   across spokes, and has already drifted (`line.trim()` everywhere except
   `roy-gateway/daemon.rs:101`; divergent unknown-event handling; the
   `ROY_SOCKET → ~/.roy/daemon.sock` fallback copied byte-identically in 6 places).

## Goal

Extract the shared wire surface into a tiny leaf crate `roy-protocol`. Spokes depend on
`roy-protocol` instead of `roy`, making the boundary **compiler-enforced**: a spoke
cannot import core internals because they are not in its dependency graph. Give the
wire codec + socket-path resolver a single canonical home (this is audit fix rank 1).

Non-goal: this does **not** address the cross-crate DB ownership leaks
(`roy-management → roy-scheduler`, `roy-auth → projects`). Those are separate tasks.

## Design

### New crate: `roy-protocol`

A leaf crate with **no** dependency on `agent-client-protocol`, `rusqlite`, `tokio`, the
daemon, or the session engine. Everything that moves here is synchronous: `parse_entry_line`
is `serde_json::from_str`, `pid_lock` uses `std::fs`, and the `wire` codec is sync — the
only `tokio::fs` usage lives in the `Journal`/`ArchivedJournal` actors, which stay in core.
Dependencies: `serde`, `serde_json`, `thiserror`, plus whatever syscall dep `pid_lock`
already uses for its liveness check (e.g. `libc`/`nix`). Modules:

| Module | Contents | Moved from |
|---|---|---|
| `error` | `RoyError`, `Result` — moved whole; the enum wraps only `io::Error`/`String`/`Duration`, no core types | `roy/src/error.rs` |
| `event` | `TurnEvent`, `StopReason` (incl. `as_wire`/`from_wire`), `event_to_json`, `event_from_json` | `roy/src/event.rs` |
| `journal` | `JournalEntry`, `Seq`, and **`parse_entry_line`** (the single JSONL→entry deserializer — part of the on-disk/wire format contract) | split from `roy/src/journal.rs` |
| `harnesses` | wire types `Harness`, `HarnessInfo`, `ModelInfo`, `HarnessesConfigStatus` (they appear inside `ServerEvent` variants) | split from `roy/src/harnesses_config.rs` |
| `control` | `ClientCommand`, `ServerEvent`, `ErrorCode`, `FireTarget`, `ConnectionSpec` | `roy/src/control.rs` |
| `pid_lock` | `PidLock`, `peek_pid`, `pid_alive`, `pid_path_for_socket` | `roy/src/pid_lock.rs` |
| `wire` ⭐ | **NEW** framing codec + `default_socket_path()` (audit fix rank 1) | — |

Dependency direction inside the crate: `control` → `event` + `journal`(types) +
`harnesses`(wire); `journal`(types) → `event`; everything → `error`. All acyclic.

### What stays in `roy` core (now depends on `roy-protocol`)

`daemon`, `manager`, `engine`, `transport` (+ ACP), `session_store`, the
`Journal`/`ArchivedJournal` **actors** (which call `roy_protocol::journal::parse_entry_line`),
and the **harness config loader** (`load_or_bootstrap`, `config_path`, `write_sample`,
`into_wire`, `SAMPLE_TOML`, `HarnessesConfig`/`HarnessEntry`/`ModelEntry`/
`HarnessesConfigError`/`LoadOutcome` — all use `tokio::fs`).

### Backward compatibility (zero churn for core, CLI, examples, core tests)

`roy/src/lib.rs` changes its `pub use` re-exports to pull from `roy_protocol`, and the
two **split** modules stay as thin wrapper modules in `roy`:

- `roy::journal` → `pub use roy_protocol::journal::{JournalEntry, Seq, parse_entry_line};`
  plus the `Journal`/`ArchivedJournal` actor definitions.
- `roy::harnesses_config` → `pub use roy_protocol::harnesses::{Harness, HarnessInfo, ModelInfo, HarnessesConfigStatus};`
  plus the loader.
- `roy::{control, event, pid_lock, wire}` → `pub use roy_protocol::{...}`.

Result: `roy::ClientCommand`, `roy::journal::JournalEntry`, `roy::Harness`,
`roy::event::TurnEvent`, etc. resolve unchanged. `roy-cli` (needs `Daemon`), examples,
and core integration tests are **not edited**.

### Spoke migration (the one-time churn)

For each of `roy-mcp`, `roy-scheduler`, `roy-gateway`, `roy-management`, `roy-inbound`:

- `Cargo.toml`: replace `roy = { path = "../roy" }` with `roy-protocol = { path = "../roy-protocol" }`.
- Source: `use roy::X` → `use roy_protocol::X` (and `roy::module::X` → `roy_protocol::module::X`).

The audit verified every spoke import from `roy` is wire surface, so each maps onto
`roy-protocol` with no missing symbol. `roy-cli` keeps its `roy` dependency unchanged.

### The `wire` module (audit fix rank 1 + 2 payload)

```rust
// roy-protocol::wire — single source of truth for the newline-JSON framing
pub fn encode_line<T: Serialize>(v: &T) -> Result<Vec<u8>>;        // serde_json::to_string + b'\n'
pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<T>;  // trim() + serde_json::from_str
pub fn default_socket_path() -> PathBuf;                           // $ROY_SOCKET else ~/.roy/daemon.sock
```

Route the daemon's `dispatch_lines` / `line_writer_loop` and all five spoke clients
plus `roy-cli` through these. This single-sources the framing, kills the trim/no-trim
drift, and makes the server codec unit-testable independently of the accept loop.

**Explicitly out of `wire`'s scope:**
- `gateway/ws.rs` transparent byte relay — it pumps `Message::Text ↔ \n` verbatim and
  never decodes to typed values; a typed codec correctly does not subsume it. Leave it.
- The "unknown ServerEvent: skip vs reject" policy is **not** unified — it is legitimately
  per-consumer (a UI relay skips unknowns; a strict client rejects). Only the framing
  mechanics are centralized; the decode-error policy stays at each call site.

### Rank-2 sub-fix (bundled, cheap)

Replace the six in-scope `format!("{stop_reason:?}")` sites (`roy-cli` main.rs:1005,1077;
`roy-mcp` lib.rs:414,505; `roy-inbound` daemon_client.rs:82; `roy-scheduler`
roy_client.rs:87) with `stop_reason.as_wire().to_string()`. **Leave**
`roy-gateway/orchestrator.rs:179` — it feeds a human-facing Telegram error footer.
Add a regression assertion on the persisted/emitted snake_case value and fix the inbound
test that hard-codes `EndTurn`.

### Documentation touch-ups (bundled)

- `CLAUDE.md`: update the stale "allowed import set" — the boundary is now "whatever
  `roy-protocol` exports", and add `roy-protocol` to the crate list.
- `control.rs` header comment that says "length-prefixed" framing → it is newline-delimited.
- `docs/architecture.md` crate layering: add `roy-protocol` as the leaf the spokes depend on.

## Migration order & verification

1. Create `roy-protocol`; move modules; make `roy` depend on it + add re-exports.
   Gate: `cargo build --workspace` green with **no** edits to core/CLI/examples/tests.
2. Migrate spokes one at a time (`mcp` → `scheduler` → `inbound` → `gateway` → `management`).
   Gate after each: `cargo build -p <crate>` + its tests.
3. Add `wire` module (TDD: round-trip `encode_line`/`decode_line`, regression for the
   trim divergence); route daemon + all clients + CLI through it; delete the 6 socket-path
   copies in favor of `default_socket_path()`.
4. Apply rank-2 `as_wire()` fix + regression assertion.
5. Doc touch-ups.
6. Full CI gate locally: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`.

## Scope boundaries (explicitly NOT in this work)

- Audit ranks 3 & 4 (cross-crate DB ownership: `management → scheduler` private SQLite;
  `roy-auth` SELECT on `projects`). Separate tasks.
- Merging or removing any crate; refactoring the `daemon.rs` god-object; improving
  intra-core TurnEvent normalization quality. Out of scope.

## Open choices

- Crate name: `roy-protocol` (proposed) vs `roy-wire` / `roy-proto`. Chosen: `roy-protocol`.
