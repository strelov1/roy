# Plan A — Roy Extensions Completion

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close two bugs in roy's already-implemented Tags / WaitForResult / Fire stack, then surface those three operations through `roy` CLI subcommands and `roy mcp` tools.

**Architecture:** The wire protocol, daemon handlers, on-disk persistence, and engine plumbing for these three extensions are already implemented (`crates/roy/src/control.rs`, `daemon.rs`, `engine.rs`, `session_meta.rs`). This plan fixes one semantic bug (set_tags upserts but spec says replace), one robustness bug (wait_for_result returns None on broadcast Lagged instead of re-scanning the journal), then adds three CLI subcommands and three MCP tools that wrap the wire commands.

**Tech Stack:** Rust 2021, `tokio`, `clap` (CLI), `serde_json` (MCP JSON-RPC). No new dependencies.

**Spec reference:** `docs/superpowers/specs/2026-05-23-background-agents-design.md` (§3 Changes to roy).

**Pre-flight read:** before editing files, skim
- `crates/roy/src/control.rs` — wire-level enums (the source of truth).
- `crates/roy/src/engine.rs` lines 1–95 (types) and 230–305 (set_tags + wait_for_result).
- `crates/roy/src/daemon.rs` lines 588–808 (handle_set_tags, handle_wait_for_result, handle_fire). Pattern to copy when adding tests.
- `crates/roy-cli/src/main.rs` — clap `Cmd` enum + dispatch + `cmd_*` functions. Add new subcommands by following `cmd_resume`/`cmd_close` shape.
- `crates/roy-cli/src/mcp.rs` — `tools_list`, `tools_call`, and existing `tool_*` functions. Follow `tool_close` shape for the simplest case, `tool_run` for the long-running case.

---

## File map

| File                                     | Why touched                                                                                         |
|------------------------------------------|-----------------------------------------------------------------------------------------------------|
| `crates/roy/src/engine.rs`               | Fix `set_tags` to replace, fix `wait_for_result` to re-scan on Lagged.                              |
| `crates/roy/src/daemon.rs` (tests mod)   | Add daemon-level tests for set_tags REPLACE and wait_for_result Lagged recovery.                    |
| `crates/roy-cli/src/main.rs`             | Add `SetTags`, `Wait`, `Fire` subcommands + their `cmd_*` impls.                                    |
| `crates/roy-cli/src/mcp.rs`              | Register `roy_set_tags`, `roy_wait_for_result`, `roy_fire` tools; add `tool_*` impls.               |

No new files. No new dependencies.

---

## Task 1: Fix `engine.set_tags` to REPLACE the tag map

**Files:**
- Modify: `crates/roy/src/engine.rs:242-251`
- Test: `crates/roy/src/daemon.rs` (`#[cfg(test)] mod tests`, end of file)

The spec (§3.1) says SetTags **replaces** the tag map — that's the only way to delete a tag. Current code upserts. Fix it and prove with a test.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block at the bottom of `crates/roy/src/daemon.rs` (the real helpers in this module are `tmp_dir`, `FakeAcpFactory`, `send_cmd_line`, `next_event_line`, and the `tokio::io::duplex` + `serve_connection` pattern — see `wait_for_result_resolves_when_turn_finishes` at line 2096 for the canonical shape):

```rust
#[tokio::test]
async fn set_tags_replaces_the_tag_map() {
    let dir = tmp_dir();
    let daemon = Arc::new(Daemon::new(dir.clone(), Arc::new(FakeAcpFactory)));

    let (client_side, server_side) = tokio::io::duplex(8192);
    let (server_rd, server_wr) = tokio::io::split(server_side);
    let _serve = {
        let d = Arc::clone(&daemon);
        tokio::spawn(async move {
            let _ = d.serve_connection(server_rd, server_wr).await;
        })
    };
    let (client_rd, mut client_wr) = tokio::io::split(client_side);
    let mut events = BufReader::new(client_rd).lines();

    // Spawn with two tags.
    let mut initial = BTreeMap::new();
    initial.insert("a".to_string(), "1".to_string());
    initial.insert("b".to_string(), "2".to_string());
    send_cmd_line(
        &mut client_wr,
        &ClientCommand::Spawn {
            agent: "opencode".into(),
            cwd: None,
            model: None,
            permission: None,
            resume: None,
            tags: initial,
        },
    )
    .await;
    let session = match next_event_line(&mut events).await {
        ServerEvent::Spawned { session, .. } => session,
        other => panic!("expected Spawned, got {other:?}"),
    };

    // SetTags with only key "b" — "a" must disappear (REPLACE, not merge).
    let mut replacement = BTreeMap::new();
    replacement.insert("b".to_string(), "new".to_string());
    send_cmd_line(
        &mut client_wr,
        &ClientCommand::SetTags {
            session: session.clone(),
            tags: replacement.clone(),
        },
    )
    .await;
    match next_event_line(&mut events).await {
        ServerEvent::SessionUpdated { tags: Some(t), .. } => {
            assert_eq!(t, replacement, "SetTags must replace, not merge");
        }
        other => panic!("expected SessionUpdated, got {other:?}"),
    }

    // Confirm List reports the replaced map too.
    send_cmd_line(&mut client_wr, &ClientCommand::List).await;
    match next_event_line(&mut events).await {
        ServerEvent::Listed { sessions } => {
            let s = sessions.iter().find(|s| s.session == session).unwrap();
            assert_eq!(s.tags, replacement);
        }
        other => panic!("expected Listed, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy --test acp_transport set_tags_replaces` (or whichever target hosts daemon tests — grep for an existing test name to confirm the harness).

Actually run from workspace:
```
cargo test --workspace set_tags_replaces -- --nocapture
```

Expected: FAIL with "SetTags must replace, not merge" because current code upserts (key "a" survives).

- [ ] **Step 3: Implement the fix**

Replace the body of `set_tags` in `crates/roy/src/engine.rs:242-251`:

```rust
/// Replace the session's tag map, persist it, and notify subscribers.
pub async fn set_tags(&self, tags: BTreeMap<String, String>) -> Result<()> {
    {
        let mut current = self.tags.lock().unwrap();
        *current = tags;
    }
    self.persist_metadata().await?;
    Ok(())
}
```

(Note: `handle_resume` at `daemon.rs:569-573` also calls `engine.set_tags` — but it does so with `Some(tags)` meaning the caller explicitly opted in, so REPLACE semantics are correct there too. The spec §3.1 says `Resume.tags = Some(map)` "upserts each key (existing keys overwritten, unmentioned keys left alone)" — to keep that contract intact, **also change `handle_resume`** to do the merge in the daemon, not in the engine.)

Replace `daemon.rs:561-586` (the `handle_resume` function) with:

```rust
async fn handle_resume(
    self: &Arc<Self>,
    session: String,
    tags: Option<BTreeMap<String, String>>,
    event_tx: &EventTx,
) {
    match self.manager.resume(&session, 256, 1024).await {
        Ok(engine) => {
            if let Some(new_tags) = tags {
                let mut merged = engine.tags();
                for (k, v) in new_tags {
                    merged.insert(k, v);
                }
                if let Err(e) = engine.set_tags(merged).await {
                    tracing::warn!(%session, error = %e, "failed to update tags on resume");
                }
            }
            let _ = event_tx.send(ServerEvent::Resumed {
                session: engine.id().to_string(),
                resume_cursor: engine.resume_cursor(),
            });
        }
        Err(e) => send_error(
            event_tx,
            Some(session),
            ErrorCode::ResumeFailed,
            e.to_string(),
        ),
    }
}
```

Same change for `handle_fire` at `daemon.rs:736-753` — replace the resume arm so it merges before calling `set_tags`:

```rust
FireTarget::Resume { session_id } => {
    match self.manager.resume(&session_id, 256, 1024).await {
        Ok(e) => {
            if !tags.is_empty() {
                let mut merged = e.tags();
                for (k, v) in tags {
                    merged.insert(k, v);
                }
                let _ = e.set_tags(merged).await;
            }
            e
        }
        Err(e) => { /* unchanged */ }
    }
}
```

(The `FireTarget::Spawn` arm at `daemon.rs:715-735` already passes tags through `SessionSpawnConfig` to spawn; no change needed there.)

- [ ] **Step 4: Run tests to verify pass**

```
cargo test --workspace -- --nocapture
```

Expected: PASS. All pre-existing tests still pass (resume-with-tags continues to merge, set-tags now replaces).

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/engine.rs crates/roy/src/daemon.rs
git commit -m "fix(engine): SetTags replaces the tag map; Resume.tags still merges

Spec §3.1 says SetTags must replace so callers can delete keys. Resume.tags
keeps upsert semantics — the merge moves into handle_resume / handle_fire
where the caller's intent is 'modify, not reset'."
```

---

## Task 2: Fix `engine.wait_for_result` to re-scan journal on broadcast Lagged

**Files:**
- Modify: `crates/roy/src/engine.rs:254-305`
- Test: `crates/roy/src/daemon.rs` (tests mod)

`engine.rs:291-296` has a known TODO: under `RecvError::Lagged`, the function returns `None`, which the caller sees as a timeout. Under scheduler load (`MAX_FIRES=8`, multiple sessions firing) Lagged is common — a phantom timeout would mark fires as failed even though the Result is sitting on disk.

Fix: on Lagged, re-replay the journal from the **last seq we saw** (not `since_seq`, or we'd re-count assistant text). Walk forward as long as new Results haven't landed, then continue listening.

- [ ] **Step 1: Write the failing test**

Pick a `broadcast_capacity` so small that flooding the engine with events while a `wait_for_result` future is suspended forces Lagged. Easiest: spawn a real session with `broadcast_capacity = 2`, queue several events synthetically, then assert the wait still resolves.

Going via the daemon is heavy. Use a unit test on `SessionEngine` directly in a new `tests` mod inside `engine.rs`. Pattern: the existing `tests/engine.rs` integration test file already drives a SessionEngine against the fake-acp-agent; copy that style.

Step 1a — extend `crates/roy/tests/scripts/fake-acp-agent.py` to support `--flood N`. Replace the top-of-file flag parsing (around line 18 `flags = set(sys.argv[1:])`) with:

```python
flags = set()
flood_n = 0
_argv = sys.argv[1:]
_i = 0
while _i < len(_argv):
    a = _argv[_i]
    if a == "--flood":
        flood_n = int(_argv[_i + 1])
        _i += 2
    else:
        flags.add(a)
        _i += 1
```

And in the `session/prompt` else-branch (around line 86, the default `chunk(sid, "ack"); result(mid, "end_turn")` case) prepend flood emission:

```python
        else:
            for _k in range(flood_n):
                chunk(sid, f"flood-{_k}\n")
            chunk(sid, "ack")
            result(mid, "end_turn")
```

Step 1b — add the test. Append to `crates/roy/tests/engine.rs` (helpers already defined in this file: `tmp_journal_dir()`, `opts(dir)`, `test_cfg()`, `fake_acp_transport_with(extra_args)`):

```rust
#[tokio::test]
async fn wait_for_result_recovers_from_broadcast_lag() {
    // Tiny broadcast capacity guarantees the wait future will Lag.
    let dir = tmp_journal_dir();
    let mut engine_opts = opts(dir.clone());
    engine_opts.broadcast_capacity = 2;

    let transport = fake_acp_transport_with(&["--flood", "50"]);
    let engine = SessionEngine::spawn(transport, engine_opts, test_cfg())
        .await
        .unwrap();

    let lease = engine.try_acquire_input().expect("free lease");
    lease.send("go").unwrap();
    drop(lease);

    let (seq, result, text) = engine
        .wait_for_result(0, Duration::from_secs(10))
        .await
        .unwrap()
        .expect("wait_for_result must recover from Lagged via journal re-scan");

    assert!(matches!(result, TurnEvent::Result { .. }));
    assert!(seq > 0);
    assert!(text.contains("flood-0"), "assistant_text must include flood prefix");
    assert!(text.contains("ack"), "assistant_text must include final chunk");

    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test --workspace wait_for_result_recovers_from_broadcast_lag -- --nocapture
```

Expected: FAIL — `wait_for_result` returns `Ok(None)` (the Lagged branch), the `.expect("...")` panics.

- [ ] **Step 3: Implement the fix**

Replace `wait_for_result` in `crates/roy/src/engine.rs:254-305`:

```rust
/// Wait for the next terminal `Result` event with `seq >= since_seq`.
/// Returns `None` only on timeout. Recovers from broadcast `Lagged`
/// (capacity overrun) by re-scanning the journal from the last seq we saw.
pub async fn wait_for_result(
    &self,
    since_seq: Seq,
    timeout: Duration,
) -> Result<Option<(Seq, TurnEvent, String)>> {
    let mut rx = self.broadcast_tx.subscribe();
    let mut scan_from = since_seq;
    let mut assistant_text = String::new();

    let fut = async {
        loop {
            // 1. Drain journal from scan_from onward. If we see Result, done.
            let entries = match self.journal.replay_from(scan_from).await {
                Ok(es) => es,
                Err(_) => return None,
            };
            let mut last_seen = scan_from;
            for entry in entries {
                last_seen = entry.seq + 1;
                match &entry.event {
                    TurnEvent::AssistantText { text } => assistant_text.push_str(text),
                    TurnEvent::Result { .. } => {
                        return Some((entry.seq, entry.event, assistant_text));
                    }
                    _ => {}
                }
            }
            scan_from = last_seen;

            // 2. Wait for the next broadcast entry. On Lagged, loop back to (1).
            match rx.recv().await {
                Ok(entry) => {
                    if entry.seq < scan_from {
                        continue;
                    }
                    scan_from = entry.seq + 1;
                    match entry.event {
                        TurnEvent::AssistantText { text } => assistant_text.push_str(&text),
                        TurnEvent::Result { .. } => {
                            return Some((entry.seq, entry.event, assistant_text));
                        }
                        _ => {}
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Re-subscribe + re-scan journal from where we left off.
                    rx = self.broadcast_tx.subscribe();
                    // assistant_text already holds everything < scan_from;
                    // the next loop iteration replays journal[scan_from..].
                    continue;
                }
            }
        }
    };

    match tokio::time::timeout(timeout, fut).await {
        Ok(res) => Ok(res),
        Err(_) => Ok(None),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test --workspace wait_for_result_recovers_from_broadcast_lag -- --nocapture
cargo test --workspace                                                              # ensure nothing else regressed
```

Expected: PASS. Existing `wait_for_result_resolves_when_result_lands` test still passes too.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/engine.rs crates/roy/tests/engine.rs tests/scripts/fake-acp-agent.py
git commit -m "fix(engine): wait_for_result recovers from broadcast Lagged via journal re-scan

Under scheduler load (MAX_FIRES=8) the per-session broadcast can lag.
The prior bail-out turned a delivered Result into a phantom timeout.
On Lagged we now re-subscribe + replay journal from the last seen seq,
which is the source of truth for both the Result and the assistant text."
```

---

## Task 3: Add `roy set-tags` subcommand

**Files:**
- Modify: `crates/roy-cli/src/main.rs` (around lines 28-50 for Cmd enum, 141-154 for dispatch, end-of-file for cmd_set_tags)

CLI surface for `SetTags`. **REPLACE semantic, matches Task 1** — `roy set-tags <session>` with no `--tag` clears all tags; each `--tag k=v` adds one to the new map.

- [ ] **Step 1: Write the failing test**

There is no existing CLI integration test harness — CLI tests run by spawning the binary. Skip an automated CLI test; rely on the daemon-level tests written in Tasks 1-2 and a manual smoke at Step 5.

Instead, write a unit test for the `--tag k=v` parser used by both `set-tags` and `fire`. Add to the end of `crates/roy-cli/src/main.rs`:

```rust
#[cfg(test)]
mod tag_parser_tests {
    use super::parse_tag_kv;

    #[test]
    fn parses_simple_kv() {
        assert_eq!(parse_tag_kv("foo=bar").unwrap(), ("foo".to_string(), "bar".to_string()));
    }

    #[test]
    fn allows_equals_inside_value() {
        assert_eq!(parse_tag_kv("k=a=b=c").unwrap(), ("k".to_string(), "a=b=c".to_string()));
    }

    #[test]
    fn rejects_empty_key() {
        assert!(parse_tag_kv("=value").is_err());
    }

    #[test]
    fn rejects_no_equals() {
        assert!(parse_tag_kv("no-equals").is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p roy-cli parses_simple_kv
```

Expected: FAIL — `parse_tag_kv` doesn't exist yet.

- [ ] **Step 3: Implement parser + subcommand**

In `crates/roy-cli/src/main.rs`:

3a. Add the parser at the end of the file (above the `#[cfg(test)]` block from Step 1):

```rust
/// Parse a CLI `--tag k=v` argument. Empty key is rejected. The first `=`
/// is the separator; subsequent `=` characters are part of the value.
pub(crate) fn parse_tag_kv(s: &str) -> anyhow::Result<(String, String)> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected k=v, got `{s}`"))?;
    if key.is_empty() {
        anyhow::bail!("tag key must not be empty (got `{s}`)");
    }
    Ok((key.to_string(), value.to_string()))
}
```

3b. Add the `SetTags` variant to the `Cmd` enum (`main.rs:28-50`):

```rust
    /// Replace the tag map on a live session. Empty `--tag` list clears all tags.
    SetTags(SetTagsArgs),
```

3c. Add the args struct after `CloseArgs`:

```rust
#[derive(clap::Args)]
struct SetTagsArgs {
    session: String,
    /// Repeatable: `--tag k=v --tag k2=v2`.
    #[arg(long = "tag", value_parser = parse_tag_kv)]
    tags: Vec<(String, String)>,
}
```

3d. Wire it into `dispatch` (`main.rs:141-154`):

```rust
        Cmd::SetTags(args) => cmd_set_tags(args).await.map(|()| ExitCode::SUCCESS),
```

3e. Implement `cmd_set_tags` after `cmd_close` (paste at end of CLI command functions, before `init_tracing`):

```rust
async fn cmd_set_tags(args: SetTagsArgs) -> anyhow::Result<()> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    let mut tags = BTreeMap::new();
    for (k, v) in args.tags {
        tags.insert(k, v);
    }

    send_cmd(
        &mut writer,
        &ClientCommand::SetTags {
            session: args.session.clone(),
            tags: tags.clone(),
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::SessionUpdated { session, tags: Some(t), .. } => {
            let payload = serde_json::json!({
                "type": "session_updated",
                "session": session,
                "tags": t,
            });
            println!("{payload}");
            Ok(())
        }
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("set-tags failed: {code}: {message}")
        }
        other => anyhow::bail!("unexpected response to SetTags: {other:?}"),
    }
}
```

- [ ] **Step 4: Run tests + verify build**

```
cargo test -p roy-cli           # parser tests pass
cargo build --workspace --all-targets   # CLI compiles with new subcommand
```

Expected: both pass.

- [ ] **Step 5: Manual smoke**

In one terminal:
```
ROY_SOCKET=/tmp/roy-smoke.sock cargo run -p roy-cli -- serve --socket /tmp/roy-smoke.sock
```

In another:
```
# spawn a fake session (any preset; we just need a session id)
echo '{"op":"spawn","agent":"fake","tags":{"a":"1","b":"2"}}' | nc -U /tmp/roy-smoke.sock | head -1
# … note the returned session id, then:
cargo run -p roy-cli -- --socket /tmp/roy-smoke.sock set-tags <session-id> --tag b=new
# expect: {"type":"session_updated","session":"<id>","tags":{"b":"new"}}   <-- "a" gone
```

(If `--socket` is not a top-level CLI flag, drop it and rely on `ROY_SOCKET` env.)

- [ ] **Step 6: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(cli): roy set-tags subcommand (replace tag map)

Empty --tag list clears all tags; each --tag k=v sets one entry on the new
map. Matches engine.set_tags REPLACE semantics from the previous commit."
```

---

## Task 4: Add `roy wait` subcommand

**Files:**
- Modify: `crates/roy-cli/src/main.rs` (Cmd enum + dispatch + cmd_wait)

`roy wait <session> [--since-seq N] [--timeout-ms M]` long-polls for the next terminal `Result` and prints the assistant text + result fields as JSON to stdout. Exit code: `0` clean Result, `1` agent-side error Result, `2` timeout / no session.

- [ ] **Step 1: Write the test**

No new unit test — coverage comes from existing daemon `wait_for_result_resolves_when_result_lands` (still passing) plus a manual smoke at Step 4.

- [ ] **Step 2: Add the subcommand**

2a. Cmd enum entry:

```rust
    /// Long-poll for the next terminal Result on a session.
    Wait(WaitArgs),
```

2b. Args struct (after `SetTagsArgs`):

```rust
#[derive(clap::Args)]
struct WaitArgs {
    session: String,
    #[arg(long)]
    since_seq: Option<u64>,
    /// Default 600_000 (10 min).
    #[arg(long)]
    timeout_ms: Option<u64>,
}
```

2c. Dispatch arm:

```rust
        Cmd::Wait(args) => cmd_wait(args).await,
```

(`cmd_wait` returns `anyhow::Result<ExitCode>`, like `cmd_run` / `cmd_attach`.)

2d. Implementation:

```rust
async fn cmd_wait(args: WaitArgs) -> anyhow::Result<ExitCode> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    send_cmd(
        &mut writer,
        &ClientCommand::WaitForResult {
            session: args.session.clone(),
            since_seq: args.since_seq,
            timeout_ms: args.timeout_ms,
        },
    )
    .await?;

    match read_event(&mut events).await? {
        ServerEvent::ResultReady { session, seq, result, assistant_text } => {
            let TurnEvent::Result { cost_usd, stop_reason } = &result else {
                anyhow::bail!("daemon sent non-Result in ResultReady: {result:?}");
            };
            let payload = serde_json::json!({
                "type": "result_ready",
                "session": session,
                "seq": seq,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            });
            println!("{payload}");
            Ok(if stop_reason.is_error() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        ServerEvent::WaitTimeout { session } => {
            let payload = serde_json::json!({
                "type": "wait_timeout",
                "session": session,
            });
            println!("{payload}");
            Ok(ExitCode::from(2))
        }
        ServerEvent::Error { code, message, .. } => {
            anyhow::bail!("wait failed: {code}: {message}");
        }
        other => anyhow::bail!("unexpected response to WaitForResult: {other:?}"),
    }
}
```

(If `StopReason::is_error` doesn't exist, copy the check pattern used in `cmd_run`'s `drain_until_terminal_result` — `event.rs` defines `StopReason`; grep there for the existing predicate.)

- [ ] **Step 3: Build**

```
cargo build --workspace --all-targets
cargo test --workspace            # no regression
```

Expected: PASS.

- [ ] **Step 4: Manual smoke**

```
# terminal 1
cargo run -p roy-cli -- serve --socket /tmp/roy-smoke.sock

# terminal 2: spawn detached, then wait for the turn
cargo run -p roy-cli -- run claude "say hi" --detach --cwd /tmp
# note the returned session id; let claude run a turn
cargo run -p roy-cli -- wait <session-id> --timeout-ms 30000
# expect: a JSON line with "type":"result_ready" and the assistant_text
```

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(cli): roy wait subcommand (long-poll for next Result)

Wraps ClientCommand::WaitForResult. Emits one JSON line on stdout
(result_ready or wait_timeout), exits 0/1/2 to match roy run conventions."
```

---

## Task 5: Add `roy fire` subcommand

**Files:**
- Modify: `crates/roy-cli/src/main.rs` (Cmd enum + dispatch + cmd_fire)

`roy fire <agent> <prompt> [--cwd P] [--resume SESS] [--tag k=v]* [--timeout-ms M]` — one-shot Spawn-or-Resume + Send + WaitForResult through the daemon's combo `Fire` command.

- [ ] **Step 1: No new unit test**

Same rationale as Task 4 — covered by daemon `fire_resolves_when_result_lands` test plus manual smoke.

- [ ] **Step 2: Add the subcommand**

2a. Cmd enum entry:

```rust
    /// One-shot fire: spawn (or resume) a session, send a prompt, wait for the result.
    Fire(FireArgs),
```

2b. Args struct:

```rust
#[derive(clap::Args)]
struct FireArgs {
    /// Required when --resume is absent.
    #[arg(value_name = "AGENT")]
    agent: Option<String>,
    /// The prompt to send. Required.
    prompt: String,
    #[arg(long, conflicts_with = "resume")]
    cwd: Option<PathBuf>,
    /// Resume an existing session id instead of spawning a new one.
    #[arg(long, conflicts_with_all = ["agent", "cwd"])]
    resume: Option<String>,
    #[arg(long = "tag", value_parser = parse_tag_kv)]
    tags: Vec<(String, String)>,
    #[arg(long)]
    timeout_ms: Option<u64>,
}
```

2c. Dispatch arm:

```rust
        Cmd::Fire(args) => cmd_fire(args).await,
```

2d. Implementation:

```rust
async fn cmd_fire(args: FireArgs) -> anyhow::Result<ExitCode> {
    use roy::FireTarget;

    let target = match (args.agent, args.resume) {
        (Some(agent), None) => FireTarget::Spawn {
            preset: agent,
            cwd: args.cwd.map(|p| p.to_string_lossy().into_owned()),
        },
        (None, Some(session_id)) => FireTarget::Resume { session_id },
        (Some(_), Some(_)) => anyhow::bail!("--resume conflicts with positional agent"),
        (None, None) => anyhow::bail!("provide either AGENT or --resume SESSION"),
    };

    let mut tags = BTreeMap::new();
    for (k, v) in args.tags {
        tags.insert(k, v);
    }

    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    send_cmd(
        &mut writer,
        &ClientCommand::Fire {
            target,
            prompt: args.prompt,
            tags,
            timeout_ms: args.timeout_ms,
        },
    )
    .await?;

    match read_event(&mut events).await? {
        ServerEvent::FireDone { session, seq_range, result, assistant_text } => {
            let TurnEvent::Result { cost_usd, stop_reason } = &result else {
                anyhow::bail!("daemon sent non-Result in FireDone: {result:?}");
            };
            let payload = serde_json::json!({
                "type": "fire_done",
                "session": session,
                "seq_range": seq_range,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            });
            println!("{payload}");
            Ok(if stop_reason.is_error() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        ServerEvent::FireTimeout { session, partial_seq_range } => {
            let payload = serde_json::json!({
                "type": "fire_timeout",
                "session": session,
                "partial_seq_range": partial_seq_range,
            });
            println!("{payload}");
            Ok(ExitCode::from(2))
        }
        ServerEvent::FireError { session, code, message } => {
            let payload = serde_json::json!({
                "type": "fire_error",
                "session": session,
                "code": code.to_string(),
                "message": message,
            });
            println!("{payload}");
            Ok(ExitCode::from(2))
        }
        other => anyhow::bail!("unexpected response to Fire: {other:?}"),
    }
}
```

2e. Re-export `FireTarget` from the `roy` crate so the CLI can use it. Edit `crates/roy/src/lib.rs:12` (the existing `pub use control::{ClientCommand, ErrorCode, ServerEvent};` line) to:

```rust
pub use control::{ClientCommand, ErrorCode, FireTarget, ServerEvent};
```

(`FireTarget` is currently only reachable via `roy::control::FireTarget`; the re-export aligns it with the other wire types.)

- [ ] **Step 3: Build + check**

```
cargo build --workspace --all-targets
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 4: Manual smoke**

```
# terminal 1
cargo run -p roy-cli -- serve --socket /tmp/roy-smoke.sock

# terminal 2
cargo run -p roy-cli -- fire claude "what is 2+2" --cwd /tmp --timeout-ms 60000 --tag run-id=smoke-1
# expect: {"type":"fire_done", ... "assistant_text":"4"}
```

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli/src/main.rs crates/roy/src/lib.rs
git commit -m "feat(cli): roy fire subcommand (Spawn|Resume + Send + WaitForResult)

The 99% scheduler call exposed for humans too. Mutually exclusive AGENT
positional vs --resume SESSION; tags via repeatable --tag k=v."
```

---

## Task 6: Add `roy_set_tags` MCP tool

**Files:**
- Modify: `crates/roy-cli/src/mcp.rs` (`tools_list` json, `tools_call` match arm, new `tool_set_tags` function)

- [ ] **Step 1: No unit test**

MCP tools have no unit tests in this codebase (see `mcp.rs` — the file has no `#[cfg(test)]`). Coverage is by integration through the live daemon. Manual smoke at Step 3.

- [ ] **Step 2: Add the tool**

2a. Append to `tools_list` (inside the array around `mcp.rs:108-180`), after `roy_close`:

```rust
            ,
            {
                "name": "roy_set_tags",
                "description": "Replace the tag map on a live session. Pass an empty `tags` object to clear all tags.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session": {"type": "string"},
                        "tags": {"type": "object", "additionalProperties": {"type": "string"}}
                    },
                    "required": ["session", "tags"],
                    "additionalProperties": false
                }
            }
```

2b. Add a match arm in `tools_call` (around `mcp.rs:188-196`):

```rust
        "roy_set_tags" => tool_set_tags(socket_path, args).await,
```

2c. Implement `tool_set_tags` (append after `tool_close`):

```rust
async fn tool_set_tags(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'session' argument"))?
        .to_string();
    let mut tags = BTreeMap::new();
    if let Some(obj) = args.get("tags").and_then(Value::as_object) {
        for (k, v) in obj {
            let val = v
                .as_str()
                .ok_or_else(|| anyhow!("tag values must be strings, got non-string for `{k}`"))?;
            tags.insert(k.clone(), val.to_string());
        }
    } else {
        return Err(anyhow!("missing 'tags' object"));
    }

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::SetTags {
            session: session.clone(),
            tags,
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::SessionUpdated { session, tags: Some(t), .. } => {
            Ok(serde_json::to_string(&json!({"session": session, "tags": t}))?)
        }
        ServerEvent::Error { code, message, .. } => {
            Err(anyhow!("set-tags failed: {code}: {message}"))
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}
```

- [ ] **Step 3: Build + manual smoke**

```
cargo build --workspace --all-targets
```

Smoke via JSON-RPC piped to `roy mcp` (treat stdin as JSON-RPC lines):

```
(
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
) | cargo run -p roy-cli -- mcp --socket /tmp/roy-smoke.sock
# Expect: initialize result, then tools/list result containing roy_set_tags.
```

- [ ] **Step 4: Commit**

```bash
git add crates/roy-cli/src/mcp.rs
git commit -m "feat(mcp): roy_set_tags tool

Wraps ClientCommand::SetTags. JSON-schema requires session + tags (object
of string→string); passing an empty tags object clears the map (replace
semantics)."
```

---

## Task 7: Add `roy_wait_for_result` MCP tool

**Files:**
- Modify: `crates/roy-cli/src/mcp.rs`

- [ ] **Step 1: No unit test**

Same as Task 6.

- [ ] **Step 2: Add the tool**

2a. Append to `tools_list`:

```rust
            ,
            {
                "name": "roy_wait_for_result",
                "description": "Long-poll for the next terminal Result on a session. Returns when a turn finishes; emits a `timeout` payload after `timeout_ms` (default 600000 = 10 min).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session": {"type": "string"},
                        "since_seq": {"type": "integer", "minimum": 0},
                        "timeout_ms": {"type": "integer", "minimum": 1}
                    },
                    "required": ["session"],
                    "additionalProperties": false
                }
            }
```

2b. Match arm:

```rust
        "roy_wait_for_result" => tool_wait_for_result(socket_path, args).await,
```

2c. Implementation:

```rust
async fn tool_wait_for_result(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'session' argument"))?
        .to_string();
    let since_seq = args.get("since_seq").and_then(Value::as_u64);
    let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64);

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::WaitForResult {
            session: session.clone(),
            since_seq,
            timeout_ms,
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::ResultReady { session, seq, result, assistant_text } => {
            let TurnEvent::Result { cost_usd, stop_reason } = result else {
                return Err(anyhow!("non-Result in ResultReady"));
            };
            Ok(serde_json::to_string(&json!({
                "type": "result_ready",
                "session": session,
                "seq": seq,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            }))?)
        }
        ServerEvent::WaitTimeout { session } => Ok(serde_json::to_string(&json!({
            "type": "wait_timeout",
            "session": session,
        }))?),
        ServerEvent::Error { code, message, .. } => {
            Err(anyhow!("wait_for_result failed: {code}: {message}"))
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}
```

- [ ] **Step 3: Build + smoke**

```
cargo build --workspace --all-targets
```

Smoke (after spawning a session via `roy run --detach`):

```
(
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"roy_wait_for_result","arguments":{"session":"<sid>","timeout_ms":30000}}}'
) | cargo run -p roy-cli -- mcp --socket /tmp/roy-smoke.sock
```

- [ ] **Step 4: Commit**

```bash
git add crates/roy-cli/src/mcp.rs
git commit -m "feat(mcp): roy_wait_for_result tool

Wraps ClientCommand::WaitForResult. Returns a JSON payload with type
result_ready (terminal Result) or wait_timeout (timeout expired)."
```

---

## Task 8: Add `roy_fire` MCP tool

**Files:**
- Modify: `crates/roy-cli/src/mcp.rs`

- [ ] **Step 1: No unit test**

Same as Tasks 6-7.

- [ ] **Step 2: Add the tool**

2a. Append to `tools_list`:

```rust
            ,
            {
                "name": "roy_fire",
                "description": "One-shot: Spawn (or Resume) a session, send a prompt, wait for the terminal Result. Returns assistant_text + stop_reason. Pass `resume` to reuse an existing session id, otherwise pass `agent` (and optional `cwd`).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {"type": "string", "enum": ["claude", "gemini", "opencode", "codex"]},
                        "cwd": {"type": "string"},
                        "resume": {"type": "string", "description": "Existing roy session id to resume into."},
                        "prompt": {"type": "string"},
                        "tags": {"type": "object", "additionalProperties": {"type": "string"}},
                        "timeout_ms": {"type": "integer", "minimum": 1}
                    },
                    "required": ["prompt"],
                    "additionalProperties": false
                }
            }
```

2b. Match arm:

```rust
        "roy_fire" => tool_fire(socket_path, args).await,
```

2c. Implementation:

```rust
async fn tool_fire(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    use roy::FireTarget;

    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'prompt'"))?
        .to_string();
    let agent = args.get("agent").and_then(Value::as_str);
    let cwd = args.get("cwd").and_then(Value::as_str).map(str::to_string);
    let resume = args.get("resume").and_then(Value::as_str);
    let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64);

    let target = match (agent, resume) {
        (Some(a), None) => FireTarget::Spawn { preset: a.to_string(), cwd },
        (None, Some(sid)) => FireTarget::Resume { session_id: sid.to_string() },
        (Some(_), Some(_)) => return Err(anyhow!("`agent` and `resume` are mutually exclusive")),
        (None, None) => return Err(anyhow!("provide either `agent` or `resume`")),
    };

    let mut tags = BTreeMap::new();
    if let Some(obj) = args.get("tags").and_then(Value::as_object) {
        for (k, v) in obj {
            let val = v.as_str().ok_or_else(|| anyhow!("tag value for `{k}` must be string"))?;
            tags.insert(k.clone(), val.to_string());
        }
    }

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::Fire { target, prompt, tags, timeout_ms },
    )
    .await?;

    match next_event(&mut lines).await? {
        ServerEvent::FireDone { session, seq_range, result, assistant_text } => {
            let TurnEvent::Result { cost_usd, stop_reason } = result else {
                return Err(anyhow!("non-Result in FireDone"));
            };
            Ok(serde_json::to_string(&json!({
                "type": "fire_done",
                "session": session,
                "seq_range": seq_range,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            }))?)
        }
        ServerEvent::FireTimeout { session, partial_seq_range } => Ok(serde_json::to_string(&json!({
            "type": "fire_timeout",
            "session": session,
            "partial_seq_range": partial_seq_range,
        }))?),
        ServerEvent::FireError { session, code, message } => Ok(serde_json::to_string(&json!({
            "type": "fire_error",
            "session": session,
            "code": code.to_string(),
            "message": message,
        }))?),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}
```

- [ ] **Step 3: Build + smoke**

```
cargo build --workspace --all-targets
cargo test --workspace
```

Smoke:

```
(
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"roy_fire","arguments":{"agent":"claude","cwd":"/tmp","prompt":"what is 2+2","timeout_ms":60000}}}'
) | cargo run -p roy-cli -- mcp --socket /tmp/roy-smoke.sock
```

- [ ] **Step 4: Commit**

```bash
git add crates/roy-cli/src/mcp.rs
git commit -m "feat(mcp): roy_fire tool

Wraps ClientCommand::Fire (combo Spawn|Resume + Send + WaitForResult).
Required: prompt. Either agent or resume must be set (exclusive)."
```

---

## Wrap-up

After all 8 tasks land:

- [ ] **Run the CI gate locally one last time**

```
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```

All three green = ready to push.

- [ ] **Update README or docs/wire-protocol.md if user-facing changes need surfacing**

Only if those files currently enumerate CLI subcommands or MCP tools. Otherwise skip.

- [ ] **Hand off**

`Plan B` (roy-scheduler crate) is now unblocked: the wire protocol behaves as the spec requires, and the scheduler can be developed entirely against the public `ClientCommand` / `ServerEvent` enums.
