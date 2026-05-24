# roy-gateway v1.1 — Streaming + /cancel + timeout test

## Goal

Three improvements to `roy-gateway` shipped together on `feature/roy-gateway-v1.1`:

1. **Streaming partial output.** Replace the `Fire` composite command with the decomposed `Spawn`/`Resume` → `AcquireInput` → `Send` → `Attach` → `ReleaseInput` flow, so the gateway can stream every `TurnEvent` into Telegram via throttled `editMessageText`. User sees thinking, tool calls, and answer text as the agent produces them instead of waiting for one final message.
2. **`/cancel` command.** Lets the user abort an in-flight turn from chat. Requires the gateway to hold the input lease — only possible in the streaming flow above.
3. **Timeout-path unit test in `orchestrator`.** Regression guard for the one `handle_message` branch that has no test today.

The order matters: timeout test is independent and ships first (commit 1). Streaming replaces the transport (commits 2–N). `/cancel` is a small handler on top of the streaming infrastructure (commit N+1).

## Why `/cancel` and streaming are coupled

`ClientCommand::CancelTurn { session }` requires the input lease, and the daemon enforces this per-connection: the cancel must come from the same connection that called `AcquireInput`. The current `Fire` flow opens its own connection inside the composite, holds the lease for the turn, and closes it — there is no way for a separate `/cancel` handler to issue `CancelTurn` against it. Decomposing the flow into explicit Spawn/Acquire/Send/Attach/Release is the prerequisite that makes `/cancel` possible.

## Out of scope

Deferred to later iterations, deliberately:

- Debounce of fast successive messages (separate iteration once user feedback demands it).
- `Channel` trait + Slack/Discord (waits for an actual second channel).
- Persisting the full transcript of the turn in the Telegram chat after edits (history lives in the roy journal, not in chat).
- Inline buttons, attachments, voice — gateway stays text-only.
- HTML/Markdown parsing of the agent's reply (raw HTML in agent text is escaped as plain text).
- Per-message rate limiting beyond the standard throttle (Telegram's API does its own).

## Architecture

### Files

```
crates/roy-gateway/src/
  daemon.rs        — REWRITTEN: TurnConn (long-lived connection holder)
                     replaces DaemonClient::fire_*; FireOutcome enum deleted.
  draft_stream.rs  — NEW: throttled HTML edit loop (1000 ms throttle,
                     250 ms floor); forceNewMessage() primitive for the
                     4096-char overflow split.
  typing.rs        — NEW: TypingKeepalive — fires sendChatAction every
                     4 s while the turn is in flight.
  cancel.rs        — NEW: CancelRegistry — Arc<DashMap<i64, CancellationToken>>
                     so the /cancel handler can signal the streaming task.
  formatting.rs    — NEW: TurnEvent → HTML render (escape <,>,& and apply
                     per-type prefix and tag wrapping).
  orchestrator.rs  — REWRITTEN: streaming pipeline replaces fire-based
                     handle_message.
  telegram.rs      — Adjusted: command-vs-text dispatch in the message
                     handler; /cancel routed to CancelRegistry.
  binder.rs        — unchanged
  config.rs        — unchanged
  main.rs          — minor: wires CancelRegistry into BotDeps.
  lib.rs           — adds new modules.
```

Total new modules: 4 (draft_stream, typing, cancel, formatting). Rewritten: 2 (daemon, orchestrator). Touched: 3 (telegram, main, lib). Unchanged: 2 (binder, config).

### Wire-format choice

- **Format**: HTML (`parseMode: HTML`). Escape `<`, `>`, `&`. Per-event prefix and wrapping (see Formatting).
- **Max message size**: 4096 chars (Telegram limit). When the accumulated draft text would exceed `MAX_SAFE = 4000`, finalize the current message and start a new one with the overflow as its initial content. The 96-char gap absorbs in-flight edits.
- **Throttle**: 1000 ms between edits to the same message; floor of 250 ms. Matches openclaw's `extensions/telegram/src/draft-stream.ts:12`.
- **Typing**: `sendChatAction(typing)` every 4 s while a turn is active. Telegram's typing indicator times out around 5 s, so 4 s is the safe re-fire interval.

### What gets shown to the user

All `TurnEvent` variants except the final terminal `Result` contribute to the rendered body. The terminal `Result` triggers a final flush + cleanup; its `stop_reason` is only surfaced if it's an error (`stop_reason.is_error()`), in which case a `⚠ {stop_reason}` footer line is appended before flush.

| `TurnEvent` variant | Rendered as |
|---------------------|-------------|
| `AssistantText { delta }` | Plain text (HTML-escaped), appended to the current text block. |
| `AssistantThought { delta }` | Italic body inside a `🧠 thinking: …` block. |
| `ToolUse { name, args }` | Standalone `🔧 <code>{name}({args})</code>` block; args truncated to ~200 chars. |
| `System { subtype }` | Standalone `<i>ℹ {subtype}</i>` block. |
| `Usage { tokens, cost }` | Standalone `📊 <code>tokens=… cost=…</code>` block. |
| `UserPrompt { … }` | Not rendered (it's our own input echoed back). |
| `Result { … }` (terminal) | Triggers flush + cleanup; on error, append `⚠ {stop_reason}` line. |
| `Raw(value)` | Standalone `⚙ <code>{compact json}</code>` block. |

### Block accumulation

`AssistantText` and `AssistantThought` arrive as a stream of incremental `{ delta }` chunks of one logical block. The renderer keeps an active *block* (a typed buffer):

- A new `delta` of the same kind extends the active block in-place.
- A `delta` of a different kind OR a non-delta event finalizes the active block (closing its HTML tags) and starts a fresh one.
- Standalone events (`ToolUse`, `System`, `Usage`, `Raw`) finalize any active block before being emitted.

Blocks are joined by `\n\n` in the rendered body for visual breathing room.

Concrete example. The agent emits, in order: `AssistantThought("Let me ")`, `AssistantThought("check the file...")`, `ToolUse("read", "main.rs")`, `AssistantText("It looks ")`, `AssistantText("fine.")`. Rendered body:

```
🧠 thinking: <i>Let me check the file...</i>

🔧 <code>read(main.rs)</code>

It looks fine.
```

The renderer's state machine is implemented in `formatting.rs`; the orchestrator just feeds it events and reads the current body string.

## Streaming pipeline (one turn)

```
on_message(text):
  1. Construct CancellationToken; CancelRegistry.insert(chat_id, token.clone())
  2. Open TurnConn (Unix socket to roy daemon)
  3. placeholder_id = replier.send(chat_id, "⏳")
  4. typing_task = TypingKeepalive::start(replier.clone(), chat_id, 4s)
  5. draft = DraftStream::new(replier.clone(), chat_id, placeholder_id,
                               throttle=1000ms, max_safe=4000)
  6. session_id = match binder.get(chat_id):
       Some(sid) => conn.resume(sid) -> Resumed { session, … }
       None      => conn.spawn(preset, project_id) -> Spawned { session, … }
  7. binder.set(chat_id, session_id)
  8. conn.acquire_input(session_id)
  9. conn.send_prompt(session_id, text)
 10. select! loop:
       Frame(event) = conn.next_frame() => {
         match event {
           Result { … } => { draft.flush(); break; }
           other        => { draft.update(buf.append(format(other))); }
         }
       }
       _ = token.cancelled() => {
         conn.cancel_turn(session_id);                 // sends CancelTurn
         draft.update(buf.append("\n\n❎ cancelled by user")); draft.flush();
         break;
       }
 11. conn.release_input(session_id)
 12. typing_task.stop()
 13. drop conn  // closes socket
 14. CancelRegistry.remove(chat_id)
```

Steps 11–14 run in a `defer` / explicit cleanup block so they execute even on early-return errors.

### Overflow split (DraftStream internals)

```
DraftStream::update(new_full_text):
  if new_full_text.len() <= max_safe:
     queue throttled edit(current_message_id, new_full_text)
     return

  // overflow: split at last good boundary
  split_at = best_boundary(new_full_text, max_safe)       // see below
  head, tail = new_full_text.split_at(split_at)
  edit(current_message_id, head); await         // finalize current message
  new_id = replier.send(chat_id, tail); current_message_id = new_id
  current_body = tail
```

`best_boundary(text, max)`: scan backward from `max` looking for `\n\n`, then `\n`, then ` `, then fall back to `max` itself. Avoids breaking mid-token visually.

After a split, subsequent `update()` calls operate against the new `current_message_id`.

## DraftStream API

```rust
pub struct DraftStream<R: Replier> {
    replier: Arc<R>,
    chat_id: i64,
    current_id: MessageId,
    current_body: String,
    pending: Option<String>,         // latest text waiting to be sent
    in_flight: Option<JoinHandle>,   // edit task currently running
    last_sent_at: Instant,
    throttle: Duration,              // 1000 ms
    floor: Duration,                 // 250 ms
    max_safe: usize,                 // 4000
}

impl<R: Replier> DraftStream<R> {
    pub fn new(replier: Arc<R>, chat_id: i64, initial_id: MessageId) -> Self;
    pub fn update(&mut self, full_body: String);      // throttled, returns immediately
    pub async fn flush(&mut self);                    // waits for in-flight + final edit
}
```

Internally maintains a small task that wakes on throttle expiry, dequeues `pending`, runs the edit, and re-arms.

## TypingKeepalive API

```rust
pub struct TypingKeepalive {
    handle: JoinHandle<()>,
}

impl TypingKeepalive {
    pub fn start<R: Replier>(replier: Arc<R>, chat_id: i64, interval: Duration) -> Self;
    pub fn stop(self);    // aborts the join handle
}
```

The internal task: `loop { replier.typing(chat_id).await.log_warn_on_err(); sleep(4s); }`. No circuit breaker; errors stay warn-logged and don't halt the loop.

## CancelRegistry API

```rust
pub struct CancelRegistry {
    inner: DashMap<i64, CancellationToken>,
}

impl CancelRegistry {
    pub fn new() -> Arc<Self>;
    pub fn register(&self, chat_id: i64) -> CancellationToken;  // inserts a fresh token, returns clone
    pub fn signal(&self, chat_id: i64) -> bool;                  // true if a turn was registered
    pub fn release(&self, chat_id: i64);                         // remove on turn end
}
```

`tokio_util::sync::CancellationToken` is idempotent: a second `/cancel` while the first is still being processed is harmless.

## TurnConn API (daemon.rs rewrite)

`TurnConn` owns one Unix-socket connection for the duration of one turn. All methods that send a `ClientCommand` and expect a response are async.

```rust
pub struct TurnConn {
    socket_path: PathBuf,
    write_half: OwnedWriteHalf,
    lines: Lines<BufReader<OwnedReadHalf>>,
}

impl TurnConn {
    pub async fn open(socket_path: &Path) -> Result<Self>;
    pub async fn spawn(&mut self, preset: &str, project_id: Option<String>,
                       tags: BTreeMap<String, String>) -> Result<String>;
    pub async fn resume(&mut self, session_id: &str,
                        tags: BTreeMap<String, String>) -> Result<String>;
    pub async fn acquire_input(&mut self, session: &str) -> Result<()>;
    pub async fn send_prompt(&mut self, session: &str, text: String) -> Result<()>;
    pub async fn next_frame(&mut self) -> Result<Option<TurnEvent>>;  // see "next_frame contract" below
    pub async fn cancel_turn(&mut self, session: &str) -> Result<()>;
    pub async fn release_input(&mut self, session: &str) -> Result<()>;
}
```

Internally each method writes one `ClientCommand` JSON line and reads back the expected `ServerEvent` (`Spawned`, `Resumed`, `InputAcquired`, etc.) or `ServerEvent::Frame` for `next_frame`. Method bodies stay short and focused on protocol mapping — the same line-framing pattern as the v1 `fire_via_stream`.

### `next_frame` contract

`next_frame` returns `Ok(Some(event))` for each `ServerEvent::Frame` received from the daemon — INCLUDING the terminal `Frame { entry: { event: Result { … } } }`. After surfacing the terminal `Result`, the next call to `next_frame` returns `Ok(None)` to signal "stream exhausted". The pipeline loops:

```rust
while let Some(event) = conn.next_frame().await? {
    match event {
        TurnEvent::Result { stop_reason, .. } => {
            if stop_reason.is_error() {
                buf.append_error_footer(&stop_reason);
            }
            draft.flush().await;
            break;
        }
        other => { buf.feed(other); draft.update(buf.body()); }
    }
}
```

This shape lets the pipeline access the `stop_reason` (for the error footer) AND treat stream end as clean.

### Lease and lifecycle

`Drop` for `TurnConn` is a no-op; closing the socket is enough for the daemon to release any held lease. The explicit `release_input` is preferred on the happy path because it lets the daemon log the clean exit.

## Orchestrator rewrite

```rust
pub trait Conn: Send {                // replaces Fire trait
    async fn spawn(&mut self, preset, project_id, tags) -> Result<String>;
    async fn resume(&mut self, session_id, tags) -> Result<String>;
    async fn acquire_input(&mut self, session) -> Result<()>;
    async fn send_prompt(&mut self, session, text) -> Result<()>;
    async fn next_frame(&mut self) -> Result<Option<TurnEvent>>;
    async fn cancel_turn(&mut self, session) -> Result<()>;
    async fn release_input(&mut self, session) -> Result<()>;
}

pub trait Replier: Send + Sync {       // grows from v1
    async fn send(&self, chat_id, html) -> Result<MessageId>;
    async fn edit(&self, chat_id, message_id, html) -> Result<()>;
    async fn typing(&self, chat_id) -> Result<()>;
}

pub trait ConnFactory: Send + Sync {   // so handle_message can open per-turn
    type Conn: Conn;
    async fn open(&self) -> Result<Self::Conn>;
}

pub async fn handle_message<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    cancel_registry: &CancelRegistry,
    conn_factory: &F,
    replier: &R,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: ConnFactory,
    R: Replier,
```

`handle_message` performs the 14-step pipeline above. Errors at any step append a `⚠` line to the draft and let the cleanup tail run.

## /cancel handler

```rust
async fn on_cancel(replier, cancel_registry, chat_id):
    if cancel_registry.signal(chat_id):
        replier.send(chat_id, "❎ cancelled").await
    else:
        replier.send(chat_id, "Нечего отменять — turn не запущен").await
```

`telegram.rs` dispatches inbound messages: if `text == "/cancel"` (allowing optional `@botname` suffix), call `on_cancel`. Otherwise call `handle_message`.

## Error handling

| Failure | Behavior |
|---------|----------|
| Daemon disconnect mid-stream | `next_frame` returns `Err`; append `⚠ connection lost`; flush; cleanup. |
| `spawn`/`resume` fail | Reply `⚠ <code>: <message>`; no `binder.set`; cleanup. |
| `acquire_input` returns `acquired=false` | Reply `⚠ session busy — try again later`; cleanup. |
| Edit fails with `retry_after` | Sleep and retry once; on second fail, finalize old + new send. |
| Edit fails with `message_too_old` | Same: finalize, send fresh. |
| `cancel_turn` returns `NoLease` (race) | Silent log; no extra reply (user already saw `❎ cancelled`). |
| `release_input` fails | Warn log; cleanup proceeds. |
| Typing keepalive `sendChatAction` fails | Warn log; loop continues. |
| Unknown `ServerEvent` on `next_frame` | Log unknown variant; treat as no-op frame; continue. |

## Testing

- **timeout-path test** in `orchestrator::tests` mod — added in commit 1, before any other change. Mirrors the existing 4 tests (sets `on_spawn` to return `FireOutcome::Timeout`, asserts replier got the ⏱ text and binder is updated). After commit 2 (streaming refactor) this test will be reshaped to fit the new pipeline; that reshape is part of commit 2.
- **DraftStream** unit tests: throttle window respected, overflow split triggers, `flush()` produces final edit, no edit fired before `update()` is called.
- **TypingKeepalive** unit tests: start fires tick periodically, stop halts further ticks, tick error doesn't break the loop.
- **CancelRegistry** unit tests: register-then-signal returns true; signal without register returns false; release removes.
- **formatting** unit tests: each `TurnEvent` variant renders to expected HTML; HTML escape applied; truncation at long arg values.
- **TurnConn** unit tests: against `tokio::io::duplex` mock daemon, exercise spawn→acquire→send→next_frame(Frame)→next_frame(Result terminal)→release sequence; assert correct `ClientCommand` JSON written at each step. Same pattern as the v1 `fire_via_stream` tests.
- **Orchestrator streaming pipeline** integration test: mock `Conn` produces a scripted sequence of `Spawned → InputAcquired → Frame(AssistantText) ×3 → Frame(Result) (terminal)`; mock `Replier` records every send/edit/typing call. Assertions: exactly one `send` for placeholder, ≥1 `edit` for streamed content, final edit contains the full body, `binder.set` called once with the returned session id.
- **/cancel orchestration test**: mock `Conn` blocks on `next_frame`; signal `CancelRegistry`; assert `conn.cancel_turn` is called and `release_input` happens.

No new integration tests against real teloxide or a real daemon — same boundary as v1.

## Migration / breaking changes

This iteration drops `FireOutcome`, `DaemonClient::fire_spawn`, `DaemonClient::fire_resume`, and `crate::daemon::fire_via_stream`. Nothing outside `roy-gateway` uses these symbols (verified). The `roy-scheduler` crate uses its own inline `execute_fire` implementation against the wire — that crate is unchanged by this iteration.

The `Fire` trait in `orchestrator.rs` is replaced by `Conn` + `ConnFactory`. Same shape (trait-based seam for unit testing) but the methods reflect the decomposed protocol.

## Commit shape (preview, fixed in writing-plans)

1. `test(roy-gateway): cover timeout outcome in orchestrator`
2. `refactor(roy-gateway): introduce Conn + ConnFactory traits, gut FireOutcome`
3. `feat(roy-gateway): TurnConn — long-held daemon connection per turn`
4. `feat(roy-gateway): DraftStream — throttled HTML edits with overflow split`
5. `feat(roy-gateway): TypingKeepalive`
6. `feat(roy-gateway): formatting — TurnEvent → HTML renderer`
7. `feat(roy-gateway): CancelRegistry`
8. `feat(roy-gateway): streaming pipeline in handle_message`
9. `feat(roy-gateway): /cancel command handler`
10. `docs(roy-gateway): README + smoke checklist for v1.1`

Final shape may collapse some pairs (e.g. TypingKeepalive + formatting). `writing-plans` will fix the granularity.
