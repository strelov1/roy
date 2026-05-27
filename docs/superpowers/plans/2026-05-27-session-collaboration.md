# Session-to-session collaboration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `roy ask <target>` (a thin sync-RPC wrapper over `ClientCommand::Fire`) and expose `ROY_SESSION_ID` to spawned ACP children, so a background agent can synchronously consult another agent or notify a human session and self-identify on `roy inject --source`.

**Architecture:** Two pure additions on top of existing primitives — no new wire variants, no new `TurnEvent`. `roy ask` resolves its target as either a live session id (→ `Fire { Resume }`) or an agent slug from roy-management (→ `Fire { Spawn { preset, system_prompt: agent.prompt } }`), then mirrors `cmd_fire`'s response handling. `ROY_SESSION_ID` is one `cmd.env(...)` line in `AcpTransport::open` (the `session_id` parameter is already passed in but currently unused). roy-web gets a small cosmetic change to flag `[ask]`-prefixed notes.

**Tech Stack:** Rust (Tokio, clap, reqwest, serde_json), Python (test fake), Svelte 5 + Tailwind v4.

**Spec:** `docs/superpowers/specs/2026-05-27-session-collaboration-design.md`.

---

## File Map

**roy (Rust crate)**
- Modify: `crates/roy/src/transport/acp/mod.rs` (drop `_` prefix on `session_id`; add `cmd.env("ROY_SESSION_ID", session_id)`)
- Modify: `crates/roy/tests/scripts/fake-acp-agent.py` (new `--env-out PATH` flag)
- Modify: `crates/roy/tests/acp_transport.rs` (new hermetic test asserting the env var is propagated)
- Modify: `crates/roy/CLAUDE.md` (one new bullet under a "Session-to-session collaboration" heading)

**roy-cli (Rust crate)**
- Modify: `crates/roy-cli/src/main.rs` (new `Ask(AskArgs)` Cmd variant, `AskArgs` struct, `cmd_ask` async fn, `build_ask_prompt` helper + its `#[cfg(test)]` unit tests)

**roy-web (Svelte SPA, sibling repo `../roy-web`)**
- Modify: `../roy-web/src/lib/components/MessageGroups.svelte` (amber-chip variant when `Note.text` begins with `[ask]`)

---

## Phase A: `ROY_SESSION_ID` env var

### Task 1: Extend the fake ACP agent with `--env-out`

**Files:**
- Modify: `crates/roy/tests/scripts/fake-acp-agent.py`

- [ ] **Step 1: Add the new flag's argv parsing**

Open `crates/roy/tests/scripts/fake-acp-agent.py`. The existing argv loop already supports `--meta-out PATH`. Add a parallel `--env-out PATH` branch that captures the path. Right after the `meta_out` variable initialization near the top:

Find:
```python
meta_out = None
_argv = sys.argv[1:]
_i = 0
while _i < len(_argv):
    a = _argv[_i]
    if a == "--flood":
        flood_n = int(_argv[_i + 1])
        _i += 2
    elif a == "--meta-out":
        meta_out = _argv[_i + 1]
        _i += 2
    else:
        flags.add(a)
        _i += 1
```

Replace with:
```python
meta_out = None
env_out = None
_argv = sys.argv[1:]
_i = 0
while _i < len(_argv):
    a = _argv[_i]
    if a == "--flood":
        flood_n = int(_argv[_i + 1])
        _i += 2
    elif a == "--meta-out":
        meta_out = _argv[_i + 1]
        _i += 2
    elif a == "--env-out":
        env_out = _argv[_i + 1]
        _i += 2
    else:
        flags.add(a)
        _i += 1
```

- [ ] **Step 2: Add `os` to the imports at the top of the file**

Find the existing import line near the top:
```python
import sys, json
```

Replace with:
```python
import sys, json, os
```

- [ ] **Step 3: Write the env value on startup**

Find the existing `record_meta` helper:
```python
def record_meta(m):
    if meta_out is not None:
        with open(meta_out, "w") as f:
            json.dump(m.get("params", {}).get("_meta"), f)
```

Immediately after it (still at module top level), add:

```python
def record_env():
    if env_out is not None:
        with open(env_out, "w") as f:
            json.dump({"ROY_SESSION_ID": os.environ.get("ROY_SESSION_ID")}, f)


record_env()
```

The unconditional call writes the file at process start, before the first JSON-RPC turn — so the test can read it after `Transport::open` returns.

- [ ] **Step 4: Sanity-check the script**

Run: `python3 crates/roy/tests/scripts/fake-acp-agent.py --env-out /tmp/roy_env_smoke.json --no-initialize-reply </dev/null & sleep 0.2; kill %1 2>/dev/null; cat /tmp/roy_env_smoke.json; rm -f /tmp/roy_env_smoke.json`

Expected output: `{"ROY_SESSION_ID": null}` (the env var isn't set yet because we haven't done Task 3; that's the failing case Task 2 will assert against and Task 3 will fix).

- [ ] **Step 5: Commit**

```bash
git add crates/roy/tests/scripts/fake-acp-agent.py
git commit -m "test(roy): fake-acp-agent --env-out flag for ROY_SESSION_ID assertion"
```

---

### Task 2: Failing test for `ROY_SESSION_ID` propagation

**Files:**
- Modify: `crates/roy/tests/acp_transport.rs`

- [ ] **Step 1: Locate the existing test pattern**

Run: `grep -n "fn .*_test\|tokio::test\|fake-acp-agent" crates/roy/tests/acp_transport.rs | head -20`

Note the in-file conventions (helper for building `AcpConfig`, how the fake script's path is set, how transports are opened). The first test in the file is the template to mirror.

- [ ] **Step 2: Append a new test at end of file**

The file already imports `roy::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport}` and defines a `fake_config(extra: &[&str]) -> AcpConfig` helper (around line 8) that wraps `tests/scripts/fake-acp-agent.py`. Reuse the helper.

Append at end of file:

```rust
#[tokio::test]
async fn open_propagates_roy_session_id_env() {
    let env_out = tempfile::NamedTempFile::new().unwrap();
    let env_out_path = env_out.path().to_path_buf();
    let env_out_str = env_out_path.to_string_lossy().to_string();

    let cfg = fake_config(&["--env-out", &env_out_str]);
    let transport = AcpTransport::new(cfg);

    let session_id = "test-session-id-abc123";
    let _handle = transport
        .open(
            session_id,
            None,
            std::env::current_dir().unwrap(),
            None,
        )
        .await
        .expect("open should succeed");

    // The fake agent wrote the env var to disk at process start, before
    // the JSON-RPC initialize handshake completes — so by the time `open`
    // returns, the file is on disk.
    let body = std::fs::read_to_string(&env_out_path).expect("env-out file should exist");
    let v: serde_json::Value = serde_json::from_str(&body).expect("env-out should be JSON");
    assert_eq!(
        v["ROY_SESSION_ID"].as_str(),
        Some(session_id),
        "expected ROY_SESSION_ID to be set to the host session id; got {v}"
    );
}
```

If `tempfile` is not a dev-dep yet, add it:

```bash
cargo add --package roy --dev tempfile
```

(check first: `grep -A 5 '\[dev-dependencies\]' crates/roy/Cargo.toml`.)

- [ ] **Step 3: Run the test, verify it fails**

Run: `cargo test -p roy --test acp_transport open_propagates_roy_session_id_env -- --nocapture`

Expected: FAIL with `expected ROY_SESSION_ID to be set to the host session id; got {"ROY_SESSION_ID":null}`.

- [ ] **Step 4: Do NOT commit yet** — committing red is fine within the next task's commit. Move on to Task 3.

---

### Task 3: Implement `ROY_SESSION_ID` env var

**Files:**
- Modify: `crates/roy/src/transport/acp/mod.rs:182-211`

- [ ] **Step 1: Drop the `_` prefix on the param**

Find at `crates/roy/src/transport/acp/mod.rs:183-189`:

```rust
async fn open(
    &self,
    _session_id: &str,
    resume_cursor: Option<&str>,
    cwd: PathBuf,
    system_prompt: Option<&str>,
) -> Result<Box<dyn Handle>> {
```

Replace `_session_id` with `session_id`. The leading underscore was a "this is intentionally unused" marker; it's about to be used.

- [ ] **Step 2: Set the env var before spawning**

Find at `crates/roy/src/transport/acp/mod.rs:202-211`:

```rust
let mut cmd = Command::new(&self.config.command);
cmd.args(&self.config.args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .kill_on_drop(true);
for key in &self.config.env_remove {
    cmd.env_remove(key);
}
let mut child = cmd.spawn().map_err(RoyError::Io)?;
```

Insert a single line after the `env_remove` loop, before `cmd.spawn()`:

```rust
for key in &self.config.env_remove {
    cmd.env_remove(key);
}
cmd.env("ROY_SESSION_ID", session_id);
let mut child = cmd.spawn().map_err(RoyError::Io)?;
```

This goes after `env_remove` deliberately — explicitly removed keys shouldn't accidentally pull `ROY_SESSION_ID` out from under us.

- [ ] **Step 3: Run the test, verify it passes**

Run: `cargo test -p roy --test acp_transport open_propagates_roy_session_id_env -- --nocapture`

Expected: PASS.

- [ ] **Step 4: Run the full transport test suite to confirm no regressions**

Run: `cargo test -p roy --test acp_transport`

Expected: all non-`#[ignore]` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/transport/acp/mod.rs crates/roy/tests/acp_transport.rs
git commit -m "feat(roy): set ROY_SESSION_ID env var on spawned ACP child

Lets agent processes self-identify their roy session id from inside their
Bash tool (e.g. \`roy inject <other> \"...\" --source \$ROY_SESSION_ID\`),
without the orchestrator having to template it into the prompt."
```

---

## Phase B: `roy ask`

### Task 4: `build_ask_prompt` helper + unit tests

**Files:**
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Write the failing tests first**

At the bottom of `crates/roy-cli/src/main.rs`, append:

```rust
#[cfg(test)]
mod tests {
    use super::build_ask_prompt;

    #[test]
    fn build_ask_prompt_without_context_is_plain_prompt() {
        assert_eq!(build_ask_prompt("do the thing", None), "do the thing");
    }

    #[test]
    fn build_ask_prompt_with_context_concatenates_with_labels() {
        let p = build_ask_prompt("Is this OK?", Some("Found a possible match in row 7."));
        assert_eq!(
            p,
            "Context:\nFound a possible match in row 7.\n\nQuestion/Task:\nIs this OK?"
        );
    }

    #[test]
    fn build_ask_prompt_empty_context_is_treated_as_no_context() {
        assert_eq!(build_ask_prompt("hi", Some("")), "hi");
    }
}
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p roy-cli build_ask_prompt`

Expected: FAIL with `cannot find function 'build_ask_prompt' in this scope`.

- [ ] **Step 3: Implement the helper**

Above the `#[cfg(test)]` block (i.e. at module top-level, anywhere convenient — near other helpers like `cmd_inject`), add:

```rust
/// Builds the prompt sent to the target agent in `roy ask`. With no
/// context, the prompt is forwarded verbatim. With context, both are
/// concatenated under explicit labels — the LLM-side equivalent of
/// CrewAI's `(task, context)` tool schema.
fn build_ask_prompt(prompt: &str, context: Option<&str>) -> String {
    match context {
        Some(ctx) if !ctx.is_empty() => {
            format!("Context:\n{ctx}\n\nQuestion/Task:\n{prompt}")
        }
        _ => prompt.to_string(),
    }
}
```

- [ ] **Step 4: Run, verify all three tests pass**

Run: `cargo test -p roy-cli build_ask_prompt`

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): build_ask_prompt helper for roy ask context concatenation"
```

---

### Task 5: `Ask` clap subcommand wiring + stub

**Files:**
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Add the `Ask` variant to the `Cmd` enum**

Find the `enum Cmd` block (`crates/roy-cli/src/main.rs:~30-98`). Locate the `Inject(InjectArgs),` line. Immediately after it, insert:

```rust
    /// Synchronously ask another session or agent persona for a text
    /// answer. Resolves `<target>` to a live session id (→ Fire Resume)
    /// or, failing that, to an agent slug from roy-management
    /// (→ Fire Spawn with that persona).
    Ask(AskArgs),
```

- [ ] **Step 2: Add the `AskArgs` struct**

Find the `struct InjectArgs` definition (`crates/roy-cli/src/main.rs:~237-247`). Immediately after that struct's closing `}`, add:

```rust
#[derive(clap::Args)]
struct AskArgs {
    /// Target: a live roy session id, or an agent slug/id from
    /// roy-management (`roy agents list`).
    target: String,
    /// The question or task text.
    prompt: String,
    /// Optional extra context, concatenated under a "Context:" label.
    #[arg(long)]
    context: Option<String>,
    /// roy-management base URL for agent-slug resolution. Falls back
    /// to $ROY_MANAGEMENT_URL.
    #[arg(
        long = "mgmt-url",
        env = "ROY_MANAGEMENT_URL",
        default_value = "http://127.0.0.1:8079"
    )]
    mgmt_url: String,
    /// Hard cap on the round-trip. Default 600_000 (10 min), same as Fire.
    #[arg(long)]
    timeout_ms: Option<u64>,
}
```

- [ ] **Step 3: Wire dispatch in `dispatch`**

Find the dispatch match (`crates/roy-cli/src/main.rs:~422-450`). Right after `Cmd::Inject(args) => cmd_inject(args).await,`, insert:

```rust
        Cmd::Ask(args) => cmd_ask(args).await,
```

- [ ] **Step 4: Add a stub `cmd_ask`**

Below `cmd_inject` (around line 906), add:

```rust
async fn cmd_ask(_args: AskArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build -p roy-cli`

Expected: PASS (warnings about unused fields in `AskArgs` are OK and disappear once Task 6 lands).

- [ ] **Step 6: Smoke-check the help output**

Run: `cargo run -p roy-cli --quiet -- ask --help`

Expected: clap-formatted help describing target/prompt/--context/--mgmt-url/--timeout-ms.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): wire \`roy ask\` clap subcommand (stub)"
```

---

### Task 6: Implement `cmd_ask` — target resolution + Fire dispatch

**Files:**
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Replace the stub `cmd_ask` with the real implementation**

Find the stub from Task 5:

```rust
async fn cmd_ask(_args: AskArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
```

Replace with:

```rust
async fn cmd_ask(args: AskArgs) -> anyhow::Result<ExitCode> {
    use roy::FireTarget;

    let final_prompt = build_ask_prompt(&args.prompt, args.context.as_deref());

    // 1. Resolve <target> — first as a live session id, then as an
    //    agent slug. If neither, the daemon-level error from Fire would
    //    be opaque, so fail fast here with a clear stderr message.
    let target = resolve_ask_target(&args.target, &args.mgmt_url).await?;

    let (mut writer, mut events) = open_daemon().await?;
    send_cmd(
        &mut writer,
        &ClientCommand::Fire {
            target,
            prompt: final_prompt,
            tags: std::collections::BTreeMap::new(),
            timeout_ms: args.timeout_ms,
        },
    )
    .await?;

    match read_event(&mut events).await? {
        ServerEvent::FireDone {
            session,
            seq_range: _,
            result,
            assistant_text,
        } => {
            let TurnEvent::Result {
                cost_usd: _,
                stop_reason,
            } = &result
            else {
                anyhow::bail!("daemon sent non-Result in FireDone: {result:?}");
            };
            let payload = serde_json::json!({
                "type": "answer",
                "session": session,
                "text": assistant_text,
            });
            println!("{payload}");
            Ok(if stop_reason.is_error() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        ServerEvent::FireTimeout { session, .. } => {
            eprintln!("roy ask: timeout (session={session})");
            Ok(ExitCode::from(2))
        }
        ServerEvent::FireError {
            session,
            code,
            message,
        } => {
            let where_ = session.unwrap_or_else(|| "<no session>".into());
            eprintln!("roy ask: {code}: {message} (session={where_})");
            Ok(ExitCode::from(2))
        }
        other => anyhow::bail!("unexpected response to Fire: {other:?}"),
    }
}

/// Resolve `<target>`: try as a live roy session id first (one
/// `ClientCommand::List` round-trip); on miss, try as an agent slug or
/// id via roy-management. Returns `Err` only on transport / HTTP
/// failure; for "unknown target" we exit 2 cleanly with a stderr message
/// rather than bubble an anyhow error.
async fn resolve_ask_target(
    target: &str,
    mgmt_url: &str,
) -> anyhow::Result<roy::FireTarget> {
    use roy::FireTarget;

    // Live-session pass.
    let (mut writer, mut events) = open_daemon().await?;
    send_cmd(&mut writer, &ClientCommand::List).await?;
    let live_match = match read_event(&mut events).await? {
        ServerEvent::Listed { sessions } => {
            sessions.into_iter().any(|s| s.session == target)
        }
        other => anyhow::bail!("unexpected response to List: {other:?}"),
    };
    if live_match {
        return Ok(FireTarget::Resume {
            session_id: target.to_string(),
        });
    }

    // Agent-slug fallback.
    let client = crate::management_client::ManagementClient::new(mgmt_url);
    let agents = client.list().await?;
    if let Some(agent) = agents.into_iter().find(|a| a.slug == target || a.id == target)
    {
        return Ok(FireTarget::Spawn {
            preset: agent.preset,
            system_prompt: Some(agent.prompt),
        });
    }

    eprintln!(
        "roy ask: unknown target '{target}' (not a live session id, not an agent slug or id)"
    );
    std::process::exit(2);
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p roy-cli --all-targets`

Expected: PASS. If `crate::management_client::ManagementClient` import path is wrong, check the module declaration near the top of `main.rs` — it's `mod management_client;` somewhere; adjust the path accordingly.

- [ ] **Step 3: Verify existing tests still pass**

Run: `cargo test -p roy-cli`

Expected: PASS (including the three `build_ask_prompt` tests from Task 4).

- [ ] **Step 4: Manual smoke (optional but recommended)**

Start a daemon, spawn a session, then ask it:

```bash
# In one shell:
cargo run -p roy-cli -- serve

# In another (replace SESSION_ID with output from `roy list` after running an agent):
cargo run -p roy-cli -- run --agent claude "say hi briefly" &
cargo run -p roy-cli -- list
# pick the id, then:
cargo run -p roy-cli -- ask <SESSION_ID> "what did you just say?"
```

Expected: stdout has one JSON line `{"type":"answer","session":"…","text":"…"}` with the agent's reply. Exit 0.

This requires a real ACP agent on PATH. Skip if not set up — the unit + transport tests are the gate; this is a confidence check.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): \`roy ask <target>\` over Fire { Resume | Spawn }

Resolves <target> as a live session id first (Fire Resume), then as an
agent slug/id from roy-management (Fire Spawn with persona's preset +
prompt). Prints {\"type\":\"answer\",\"session\":..,\"text\":..} on
FireDone; exit 0/1 on success/stop-reason-error, exit 2 on timeout /
fire error / unknown target."
```

---

## Phase C: roy-web cosmetic + docs

### Task 7: roy-web amber chip for `[ask]`-prefixed notes

**Files:**
- Modify: `../roy-web/src/lib/components/MessageGroups.svelte:317-337`

- [ ] **Step 1: Update the Note branch**

Open `../roy-web/src/lib/components/MessageGroups.svelte`. Find the `{:else if item.kind === 'note'}` branch (around line 317-337). Replace the entire `<article>` element with a version that switches styling on an `[ask]` prefix:

```svelte
      {:else if item.kind === 'note'}
        {@const isAsk = item.text.startsWith('[ask]')}
        <article
          class={[
            'self-stretch rounded-lg px-4 py-2.5 text-sm',
            isAsk
              ? 'border border-amber-400/40 bg-amber-400/5'
              : 'border border-primary/30 bg-primary/5',
          ].join(' ')}
        >
          <div
            class={[
              'mb-1 flex items-center gap-1 text-[0.65rem] font-semibold uppercase tracking-wider',
              isAsk ? 'text-amber-600' : 'text-primary/80',
            ].join(' ')}
          >
            <span>{isAsk ? 'awaiting reply' : 'background'}{#if item.sourceSession}
              {@const src = item.sourceSession}
              ·
              <a
                class="underline"
                href={`/s/${src}`}
                onclick={(e) => {
                  if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey) return;
                  e.preventDefault();
                  void app.openSession(src);
                  window.history.pushState(null, '', `/s/${src}`);
                }}
              >{src.slice(0, 8)}</a>
            {/if}</span>
            {@render timeBadge(item.ts_ms, 'ml-auto font-normal normal-case tracking-normal text-muted-foreground')}
          </div>
          <pre class="m-0 whitespace-pre-wrap break-words font-sans">{item.text}</pre>
        </article>
```

The `[ask]` prefix is a soft convention — the wire stays the same; only the UI distinguishes the two flavors.

- [ ] **Step 2: Type-check the change**

Run (from `../roy-web`): `npm run check`

Expected: PASS (or whatever the project's TS/Svelte type-check command is — read `package.json` `scripts` to confirm).

- [ ] **Step 3: Visual smoke**

Run the dev server (from `../roy-web`): `npm run dev`

In a separate shell on the roy daemon: inject a regular note (`roy inject <session> "regular"`) and an ask-prefixed note (`roy inject <session> "[ask] should I do X?"`). In the browser, confirm the first renders with the primary chip and the second renders with the amber `awaiting reply` chip. Click the underlined source link on both; both should still navigate to the source session.

- [ ] **Step 4: Commit (in roy-web's repo)**

```bash
cd ../roy-web
git add src/lib/components/MessageGroups.svelte
git commit -m "feat(chat): amber 'awaiting reply' chip for [ask]-prefixed notes"
```

---

### Task 8: CLAUDE.md doc note

**Files:**
- Modify: `crates/roy/CLAUDE.md`

- [ ] **Step 1: Insert a "Session-to-session collaboration" section**

Open `crates/roy/CLAUDE.md`. After the "Per-scope cwd layout" section (it ends with the paragraph beginning "The daemon remains trusted: ..."), insert:

```markdown
### Session-to-session collaboration

Two patterns sit on top of existing primitives, no new wire variants:

- **Agent asks human.** A background agent runs
  `roy inject <human_session> "<question>" --source $ROY_SESSION_ID`.
  The daemon sets `ROY_SESSION_ID` on every spawned ACP child
  (`transport/acp/mod.rs` `AcpTransport::open`), so the agent can pass
  its own session id without the orchestrator templating it in. The
  human's roy-web renders the `Note` with a clickable link back to the
  asker's session (`MessageGroups.svelte`); the human navigates there
  and types a reply, which goes to the agent as a normal `Cmd::Prompt`.
- **Agent asks agent.** A background agent runs
  `roy ask <target> "<prompt>" [--context "..."] [--timeout 10m]`.
  `<target>` resolves to a live roy session id (→ `Fire { Resume }`) or
  an agent slug/id from roy-management (→ `Fire { Spawn { preset,
  system_prompt: agent.prompt } }`). The CLI blocks on `Fire`, prints
  `{"type":"answer","session":..,"text":..}` on `FireDone`, and exits
  0 / 1 / 2 just like `roy fire`.

Both flows are sync from the agent's perspective. Neither introduces a
pending-question store, a new `TurnEvent`, or a new `ClientCommand`.
```

- [ ] **Step 2: Commit**

```bash
git add crates/roy/CLAUDE.md
git commit -m "docs(roy): describe session-to-session collaboration patterns"
```

---

## Final gate

- [ ] **Step 1: Workspace check matches CI**

Run, in this order:

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```

All three must be green. Any failure → fix at root cause (not by skipping the failing test); follow the CLAUDE.md "Code quality bar" guidance.

- [ ] **Step 2: Sanity-check the new commands integrate cleanly**

Run: `cargo run -p roy-cli --quiet -- --help | grep -E "^\s+(ask|inject)"`

Expected: both `ask` and `inject` appear in the subcommand list.

- [ ] **Step 3: Done**

Nothing remains. The two patterns (agent-asks-human, agent-asks-agent) are usable from any agent's Bash tool. The Telegram and inbound paths inherit the same behavior — no per-channel work.
