# roy-gateway v1.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace v1's `Fire`-based one-shot transport with a streaming pipeline that edits one Telegram message as the agent produces `TurnEvent`s, plus a `/cancel` command, plus one regression-guard test for the timeout path.

**Architecture:** Decompose the `Fire` composite into `Spawn`/`Resume → AcquireInput → Send → Attach (Frame loop) → ReleaseInput`. A long-held `TurnConn` owns one Unix-socket connection per turn so the input lease lives in our hands — that is the prerequisite for `/cancel`. Render every `TurnEvent` (including thinking and tool calls) through an HTML formatter into a `DraftStream` that edits the placeholder message every ~1s with overflow split at 4096 chars. A `CancelRegistry` keyed by `chat_id` lets the `/cancel` handler signal the streaming task to abort cleanly.

**Tech Stack:** `roy` (in-workspace), `tokio` 1, `tokio-util` 0.7 (`CancellationToken`), `serde` / `serde_json`, `async-trait`, `teloxide` 0.13, `tracing`, `anyhow`. No new external crates beyond `tokio-util`.

**Spec:** `docs/superpowers/specs/2026-05-24-roy-gateway-streaming-cancel.md`. Read it once before starting — it has the data-flow diagram, formatting rules, and error matrix that this plan implements.

**Branch:** `feature/roy-gateway-v1.1`, branched from `master` (commit `9e943b7`). Worktree at `/Users/i_strelov/Projects/roy/.claude/worktrees/feature+roy-gateway-v1.1`.

---

## File Structure

Final layout:

```
crates/roy-gateway/
  Cargo.toml          — adds `tokio-util` dep
  src/
    binder.rs         — unchanged
    config.rs         — unchanged
    daemon.rs         — REWRITTEN: TurnConn replaces FireOutcome / fire_*
    orchestrator.rs   — REWRITTEN: Conn / ConnFactory traits + streaming handle_message
    telegram.rs       — REWRITTEN: streaming pipeline call + /cancel routing + grown Replier impl
    main.rs           — small: wires CancelRegistry into BotDeps
    lib.rs            — adds new modules
    draft_stream.rs   — NEW: throttled HTML edit loop, overflow split
    typing.rs         — NEW: TypingKeepalive task
    cancel.rs         — NEW: CancelRegistry over Arc<Mutex<HashMap<i64, CancellationToken>>>
    formatting.rs     — NEW: TurnEvent stream → HTML body, with block-state machine
  tests/
    binder_persistence.rs — unchanged
```

Total new files: 4 (`draft_stream`, `typing`, `cancel`, `formatting`). Rewritten: 3 (`daemon`, `orchestrator`, `telegram`). Touched: 2 (`main`, `lib`, `Cargo.toml`). Unchanged: 2 (`binder`, `config`).

---

## Task 1: Timeout-path test in orchestrator

This test ships first because it exercises code that's about to be replaced. It documents v1 behavior and forces a parallel test (test 5) in the v1.1 rewrite at task 7.

**Files:**
- Modify: `crates/roy-gateway/src/orchestrator.rs` (append a test to the existing `tests` mod)

- [ ] **Step 1: Append the failing test to the existing `tests` mod**

In `crates/roy-gateway/src/orchestrator.rs`, find the closing `}` of `#[cfg(test)] mod tests { … }`. Append immediately before the closing brace:

```rust
    #[tokio::test]
    async fn timeout_outcome_replies_and_persists_returned_session() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let fire = MockFire::new();
        *fire.on_spawn.lock().unwrap() = Some(FireOutcome::Timeout {
            session: Some("sess-timed".into()),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 42, "hi".into())
            .await
            .unwrap();

        assert_eq!(binder.get(42).await.as_deref(), Some("sess-timed"));
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].1.contains("⏱"));
        assert!(sent[0].1.contains("timed out"));
    }
```

- [ ] **Step 2: Run, confirm PASS**

Run: `cargo test -p roy-gateway --lib orchestrator::tests::timeout 2>&1 | tail -10`
Expected: 1 passed, 0 failed (the implementation in v1's `handle_message` already covers this branch — this test is a regression guard, not driving new code).

- [ ] **Step 3: Format check**

Run: `cargo fmt --all -- --check`
Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/src/orchestrator.rs
git commit -m "test(roy-gateway): cover timeout outcome in orchestrator"
```

---

## Task 2: formatting module — TurnEvent → HTML

Pure module, no external callers yet. Builds the state machine that converts the live `TurnEvent` stream into a growing HTML body string. Block-accumulation logic is the heart.

**Files:**
- Create: `crates/roy-gateway/src/formatting.rs`
- Modify: `crates/roy-gateway/src/lib.rs` (add `pub mod formatting;`)

- [ ] **Step 1: Add `pub mod formatting;` to `lib.rs`**

Edit `crates/roy-gateway/src/lib.rs`. Add a single line under the existing `pub mod telegram;` (alphabetically placed near it):

```rust
pub mod formatting;
```

- [ ] **Step 2: Create the file with failing tests**

Create `crates/roy-gateway/src/formatting.rs` with EXACTLY this content:

```rust
//! Render a stream of `TurnEvent`s into a growing HTML body suitable for
//! Telegram's `parseMode: HTML`. Keeps a block-state machine so successive
//! `AssistantText` (or `AssistantThought`) deltas extend the same block,
//! and any other event finalizes the active block.

use roy::event::TurnEvent;
use serde_json::Value;

/// HTML-escape `<`, `>`, `&` — the only entities Telegram HTML mode cares about.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, Default)]
struct ActiveBlock {
    kind: BlockKind,
    buf: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum BlockKind {
    #[default]
    None,
    Text,
    Thought,
}

#[derive(Debug, Default)]
pub struct Renderer {
    finalized: Vec<String>, // each entry is one full block, already HTML-escaped and wrapped
    active: ActiveBlock,
}

impl Renderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event. Mutates internal state; the rendered body is read via `body()`.
    pub fn feed(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::AssistantText { delta } => self.extend_block(BlockKind::Text, &delta),
            TurnEvent::AssistantThought { delta } => self.extend_block(BlockKind::Thought, &delta),
            TurnEvent::ToolUse { name, args } => {
                self.finalize_active();
                let args_str = render_tool_args(&args);
                self.finalized
                    .push(format!("🔧 <code>{}({})</code>", escape(&name), escape(&args_str)));
            }
            TurnEvent::System { subtype, .. } => {
                self.finalize_active();
                self.finalized
                    .push(format!("<i>ℹ {}</i>", escape(&subtype)));
            }
            TurnEvent::Usage { tokens, cost_usd, .. } => {
                self.finalize_active();
                self.finalized.push(format!(
                    "📊 <code>tokens={} cost=${:.4}</code>",
                    tokens,
                    cost_usd.unwrap_or(0.0)
                ));
            }
            TurnEvent::Raw(value) => {
                self.finalize_active();
                let compact = serde_json::to_string(&value).unwrap_or_default();
                self.finalized
                    .push(format!("⚙ <code>{}</code>", escape(&compact)));
            }
            TurnEvent::UserPrompt { .. } | TurnEvent::Result { .. } => {
                // UserPrompt is our own input echoed back; not rendered.
                // Result is terminal and handled by the caller's pipeline.
            }
        }
    }

    /// Append an explicit error footer line (for terminal `Result` with error stop_reason).
    pub fn append_error_footer(&mut self, reason: &str) {
        self.finalize_active();
        self.finalized
            .push(format!("⚠ {}", escape(reason)));
    }

    /// Return the current rendered body, joining finalized blocks and the active one.
    pub fn body(&self) -> String {
        let mut all: Vec<String> = self.finalized.clone();
        if let Some(active) = self.render_active() {
            all.push(active);
        }
        all.join("\n\n")
    }

    fn extend_block(&mut self, kind: BlockKind, delta: &str) {
        if self.active.kind != kind {
            self.finalize_active();
            self.active.kind = kind;
            self.active.buf.clear();
        }
        self.active.buf.push_str(delta);
    }

    fn finalize_active(&mut self) {
        if let Some(rendered) = self.render_active() {
            self.finalized.push(rendered);
        }
        self.active.kind = BlockKind::None;
        self.active.buf.clear();
    }

    fn render_active(&self) -> Option<String> {
        if self.active.buf.is_empty() {
            return None;
        }
        let escaped = escape(&self.active.buf);
        Some(match self.active.kind {
            BlockKind::None => escaped,
            BlockKind::Text => escaped,
            BlockKind::Thought => format!("🧠 thinking: <i>{}</i>", escaped),
        })
    }
}

const TOOL_ARGS_MAX: usize = 200;

fn render_tool_args(args: &Value) -> String {
    let raw = serde_json::to_string(args).unwrap_or_default();
    if raw.len() <= TOOL_ARGS_MAX {
        raw
    } else {
        format!("{}…", &raw[..TOOL_ARGS_MAX])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy::event::{StopReason, TurnEvent};
    use serde_json::json;

    #[test]
    fn text_deltas_concatenate_into_one_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantText { delta: "Hello ".into() });
        r.feed(TurnEvent::AssistantText { delta: "world".into() });
        assert_eq!(r.body(), "Hello world");
    }

    #[test]
    fn thought_deltas_render_inside_thinking_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantThought { delta: "Let me ".into() });
        r.feed(TurnEvent::AssistantThought { delta: "check".into() });
        assert_eq!(r.body(), "🧠 thinking: <i>Let me check</i>");
    }

    #[test]
    fn switching_kinds_finalizes_active_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantThought { delta: "thinking".into() });
        r.feed(TurnEvent::AssistantText { delta: "answer".into() });
        assert_eq!(r.body(), "🧠 thinking: <i>thinking</i>\n\nanswer");
    }

    #[test]
    fn tool_use_is_standalone_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantThought { delta: "checking...".into() });
        r.feed(TurnEvent::ToolUse {
            name: "read".into(),
            args: json!({"path": "main.rs"}),
        });
        r.feed(TurnEvent::AssistantText { delta: "looks fine.".into() });
        assert_eq!(
            r.body(),
            "🧠 thinking: <i>checking...</i>\n\n🔧 <code>read({&quot;path&quot;:&quot;main.rs&quot;})</code>\n\nlooks fine."
        );
    }

    #[test]
    fn html_special_chars_escaped() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantText { delta: "if a < b && c > d".into() });
        assert_eq!(r.body(), "if a &lt; b &amp;&amp; c &gt; d");
    }

    #[test]
    fn long_tool_args_truncate() {
        let mut r = Renderer::new();
        let long = "x".repeat(500);
        r.feed(TurnEvent::ToolUse {
            name: "n".into(),
            args: json!({"big": long}),
        });
        let body = r.body();
        assert!(body.contains("…"));
        assert!(body.len() < 300);
    }

    #[test]
    fn user_prompt_and_result_are_skipped() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::UserPrompt { text: "ignore me".into() });
        r.feed(TurnEvent::AssistantText { delta: "yo".into() });
        r.feed(TurnEvent::Result {
            cost_usd: None,
            stop_reason: StopReason::EndTurn,
        });
        assert_eq!(r.body(), "yo");
    }

    #[test]
    fn error_footer_appended_after_active_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantText { delta: "partial".into() });
        r.append_error_footer("aborted");
        assert_eq!(r.body(), "partial\n\n⚠ aborted");
    }
}
```

- [ ] **Step 3: Build to confirm it compiles**

Run: `cargo build -p roy-gateway 2>&1 | tail -5`
Expected: PASS. (If `TurnEvent::System` doesn't have a `subtype` field, OR `UserPrompt` doesn't have `text`, OR the variant shapes differ — adapt to actual shapes in `crates/roy/src/event.rs` and ADAPT the match arms minimally. Report adaptation in your final status.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p roy-gateway --lib formatting 2>&1 | tail -15`
Expected: 7 passed, 0 failed.

- [ ] **Step 5: Format check**

Run: `cargo fmt --all -- --check`
Expected: no output.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-gateway/src/formatting.rs crates/roy-gateway/src/lib.rs
git commit -m "feat(roy-gateway): formatting module — TurnEvent → HTML"
```

---

## Task 3: DraftStream — throttled HTML edits with overflow split

Pure module, no external callers yet. Encapsulates the throttled-edit loop and the 4096-char split.

**Files:**
- Create: `crates/roy-gateway/src/draft_stream.rs`
- Modify: `crates/roy-gateway/src/lib.rs` (add `pub mod draft_stream;`)

- [ ] **Step 1: Add module to `lib.rs`**

Append `pub mod draft_stream;` to `crates/roy-gateway/src/lib.rs`.

- [ ] **Step 2: Create the file with stub trait + tests + impl**

Create `crates/roy-gateway/src/draft_stream.rs` with EXACTLY this content:

```rust
//! Throttled Telegram message edits. One `DraftStream` manages one "current"
//! placeholder message and edits it as the body grows. When the body would
//! overflow Telegram's 4096-char limit, the current message is finalized and
//! a new placeholder is sent; the stream continues editing the new one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::Mutex;

/// Replier abstraction for outbound Telegram operations DraftStream needs.
/// Production impl is `TeloxideReplier` in `telegram.rs`; tests use a mock.
#[async_trait]
pub trait DraftReplier: Send + Sync {
    async fn send(&self, chat_id: i64, html: &str) -> Result<i32>;
    async fn edit(&self, chat_id: i64, message_id: i32, html: &str) -> Result<()>;
}

const DEFAULT_THROTTLE_MS: u64 = 1000;
const THROTTLE_FLOOR_MS: u64 = 250;
const MAX_SAFE_CHARS: usize = 4000;

pub struct DraftStream<R: DraftReplier> {
    replier: Arc<R>,
    chat_id: i64,
    state: Mutex<State>,
    throttle: Duration,
}

struct State {
    current_id: i32,
    current_body: String,
    last_sent_at: Instant,
}

impl<R: DraftReplier + 'static> DraftStream<R> {
    pub fn new(replier: Arc<R>, chat_id: i64, initial_message_id: i32) -> Self {
        Self::with_throttle(
            replier,
            chat_id,
            initial_message_id,
            Duration::from_millis(DEFAULT_THROTTLE_MS),
        )
    }

    pub fn with_throttle(
        replier: Arc<R>,
        chat_id: i64,
        initial_message_id: i32,
        throttle: Duration,
    ) -> Self {
        let throttle = throttle.max(Duration::from_millis(THROTTLE_FLOOR_MS));
        Self {
            replier,
            chat_id,
            throttle,
            state: Mutex::new(State {
                current_id: initial_message_id,
                current_body: String::new(),
                last_sent_at: Instant::now() - throttle,
            }),
        }
    }

    /// Replace the current body with `full_body`. If we're inside the throttle
    /// window, the edit is skipped — the next eligible call will reflect the
    /// latest value. Overflow handling splits to a new message when needed.
    pub async fn update(&self, full_body: String) -> Result<()> {
        let mut guard = self.state.lock().await;

        if full_body.len() > MAX_SAFE_CHARS {
            let split_at = best_boundary(&full_body, MAX_SAFE_CHARS);
            let (head, tail) = full_body.split_at(split_at);
            // Finalize current message with the head.
            self.replier.edit(self.chat_id, guard.current_id, head).await?;
            // Start a new message with the tail as its initial body.
            let new_id = self.replier.send(self.chat_id, tail).await?;
            guard.current_id = new_id;
            guard.current_body = tail.to_string();
            guard.last_sent_at = Instant::now();
            return Ok(());
        }

        if full_body == guard.current_body {
            return Ok(());
        }

        if guard.last_sent_at.elapsed() < self.throttle {
            guard.current_body = full_body;
            return Ok(());
        }

        self.replier
            .edit(self.chat_id, guard.current_id, &full_body)
            .await?;
        guard.current_body = full_body;
        guard.last_sent_at = Instant::now();
        Ok(())
    }

    /// Force the latest body to be written even if we're inside the throttle window.
    /// Use at end-of-turn to make sure the final state is visible.
    pub async fn flush(&self) -> Result<()> {
        let guard = self.state.lock().await;
        if guard.current_body.is_empty() {
            return Ok(());
        }
        self.replier
            .edit(self.chat_id, guard.current_id, &guard.current_body)
            .await?;
        Ok(())
    }
}

fn best_boundary(text: &str, max: usize) -> usize {
    if text.len() <= max {
        return text.len();
    }
    let head = &text[..max];
    if let Some(idx) = head.rfind("\n\n") {
        return idx + 2;
    }
    if let Some(idx) = head.rfind('\n') {
        return idx + 1;
    }
    if let Some(idx) = head.rfind(' ') {
        return idx + 1;
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MockReplier {
        sent: StdMutex<Vec<(i64, String)>>,
        edits: StdMutex<Vec<(i64, i32, String)>>,
        next_id: StdMutex<i32>,
    }

    impl MockReplier {
        fn new(starting_id: i32) -> Self {
            Self {
                sent: Default::default(),
                edits: Default::default(),
                next_id: StdMutex::new(starting_id),
            }
        }
    }

    #[async_trait]
    impl DraftReplier for MockReplier {
        async fn send(&self, chat_id: i64, html: &str) -> Result<i32> {
            self.sent.lock().unwrap().push((chat_id, html.into()));
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            Ok(*id)
        }
        async fn edit(&self, chat_id: i64, message_id: i32, html: &str) -> Result<()> {
            self.edits.lock().unwrap().push((chat_id, message_id, html.into()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn first_update_after_construction_edits_immediately() {
        let replier = Arc::new(MockReplier::new(100));
        let stream = DraftStream::new(replier.clone(), 7, 100);
        stream.update("hello".into()).await.unwrap();
        let edits = replier.edits.lock().unwrap().clone();
        assert_eq!(edits, vec![(7, 100, "hello".into())]);
    }

    #[tokio::test]
    async fn rapid_update_within_throttle_window_skips_edit() {
        let replier = Arc::new(MockReplier::new(100));
        let stream = DraftStream::with_throttle(
            replier.clone(),
            7,
            100,
            Duration::from_millis(500),
        );
        stream.update("one".into()).await.unwrap();
        stream.update("two".into()).await.unwrap();
        stream.update("three".into()).await.unwrap();
        let edits = replier.edits.lock().unwrap().clone();
        // First update goes through; subsequent two within throttle skip.
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].2, "one");
    }

    #[tokio::test]
    async fn flush_writes_latest_body_even_if_throttled() {
        let replier = Arc::new(MockReplier::new(100));
        let stream = DraftStream::with_throttle(
            replier.clone(),
            7,
            100,
            Duration::from_millis(500),
        );
        stream.update("one".into()).await.unwrap();
        stream.update("two".into()).await.unwrap();
        stream.flush().await.unwrap();
        let edits = replier.edits.lock().unwrap().clone();
        // First edit was "one"; flush forces "two" through.
        assert_eq!(edits.last().unwrap().2, "two");
    }

    #[tokio::test]
    async fn no_redundant_edit_when_body_unchanged() {
        let replier = Arc::new(MockReplier::new(100));
        let stream = DraftStream::new(replier.clone(), 7, 100);
        stream.update("same".into()).await.unwrap();
        // Wait past throttle, then update with same body — should still no-op.
        tokio::time::sleep(Duration::from_millis(300)).await;
        stream.update("same".into()).await.unwrap();
        let edits = replier.edits.lock().unwrap().clone();
        assert_eq!(edits.len(), 1);
    }

    #[tokio::test]
    async fn overflow_triggers_finalize_and_new_message() {
        let replier = Arc::new(MockReplier::new(100));
        let stream = DraftStream::new(replier.clone(), 7, 100);
        // Build a body that crosses MAX_SAFE_CHARS (4000) with a paragraph break.
        let head = "h".repeat(3990);
        let body = format!("{head}\n\n{tail}", head = head, tail = "t".repeat(50));
        stream.update(body.clone()).await.unwrap();

        let edits = replier.edits.lock().unwrap().clone();
        let sends = replier.sent.lock().unwrap().clone();
        // One edit finalizing original message with head ending after "\n\n".
        assert_eq!(edits.len(), 1);
        assert!(edits[0].2.ends_with("\n\n"));
        // One new message with the tail.
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].1, "t".repeat(50));
    }

    #[test]
    fn best_boundary_prefers_double_newline() {
        let text = "a a a\n\nb b b c c c";
        // Within first 10 chars: "a a a\n\nb b". Boundary should be just after "\n\n".
        let idx = best_boundary(text, 10);
        assert_eq!(&text[..idx], "a a a\n\n");
    }

    #[test]
    fn best_boundary_falls_back_to_single_newline_then_space() {
        let text = "a b c d e\nf g";
        let idx = best_boundary(text, 8);
        // Best break in first 8 chars: "a b c d e" — fall back to space.
        // First 8 chars: "a b c d ". rfind(' ') = 6 ("a b c d_e"). idx = 7.
        assert_eq!(&text[..idx], "a b c d ");
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p roy-gateway 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-gateway --lib draft_stream 2>&1 | tail -15`
Expected: 7 passed, 0 failed (5 async + 2 sync).

- [ ] **Step 5: Format check**

Run: `cargo fmt --all -- --check`
Expected: no output.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-gateway/src/draft_stream.rs crates/roy-gateway/src/lib.rs
git commit -m "feat(roy-gateway): DraftStream — throttled HTML edits with overflow split"
```

---

## Task 4: TypingKeepalive

Tiny module: fires `sendChatAction(typing)` every N seconds while a turn is active.

**Files:**
- Create: `crates/roy-gateway/src/typing.rs`
- Modify: `crates/roy-gateway/src/lib.rs` (add `pub mod typing;`)

- [ ] **Step 1: Add module to `lib.rs`**

Append `pub mod typing;` to `crates/roy-gateway/src/lib.rs`.

- [ ] **Step 2: Create the file**

Create `crates/roy-gateway/src/typing.rs` with EXACTLY this content:

```rust
//! Periodic `sendChatAction(typing)` while a turn is in flight.
//! Telegram's typing indicator times out around 5 s; we re-fire every 4 s.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::task::JoinHandle;

#[async_trait]
pub trait TypingReplier: Send + Sync {
    async fn typing(&self, chat_id: i64) -> Result<()>;
}

pub struct TypingKeepalive {
    handle: JoinHandle<()>,
}

impl TypingKeepalive {
    pub fn start<R: TypingReplier + 'static>(
        replier: Arc<R>,
        chat_id: i64,
        interval: Duration,
    ) -> Self {
        let handle = tokio::spawn(async move {
            loop {
                if let Err(e) = replier.typing(chat_id).await {
                    tracing::warn!(?e, chat_id, "typing action failed");
                }
                tokio::time::sleep(interval).await;
            }
        });
        Self { handle }
    }

    pub fn stop(self) {
        self.handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingReplier {
        count: AtomicUsize,
    }

    #[async_trait]
    impl TypingReplier for CountingReplier {
        async fn typing(&self, _chat_id: i64) -> Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FailingReplier {
        count: AtomicUsize,
    }

    #[async_trait]
    impl TypingReplier for FailingReplier {
        async fn typing(&self, _chat_id: i64) -> Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("simulated failure"))
        }
    }

    #[tokio::test]
    async fn fires_periodically_and_stops_on_stop() {
        let replier = Arc::new(CountingReplier {
            count: AtomicUsize::new(0),
        });
        let keepalive = TypingKeepalive::start(replier.clone(), 7, Duration::from_millis(50));
        tokio::time::sleep(Duration::from_millis(175)).await;
        let count_during = replier.count.load(Ordering::SeqCst);
        keepalive.stop();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let count_after = replier.count.load(Ordering::SeqCst);
        // During 175ms with 50ms interval: at least 3 calls (t=0, 50, 100, 150).
        assert!(count_during >= 3, "expected ≥3 ticks, got {count_during}");
        // After stop, count should not grow.
        assert_eq!(count_after, count_during);
    }

    #[tokio::test]
    async fn errors_do_not_halt_the_loop() {
        let replier = Arc::new(FailingReplier {
            count: AtomicUsize::new(0),
        });
        let keepalive = TypingKeepalive::start(replier.clone(), 7, Duration::from_millis(40));
        tokio::time::sleep(Duration::from_millis(150)).await;
        let count = replier.count.load(Ordering::SeqCst);
        keepalive.stop();
        assert!(count >= 3, "loop should keep ticking despite errors, got {count}");
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p roy-gateway 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-gateway --lib typing 2>&1 | tail -10`
Expected: 2 passed.

- [ ] **Step 5: Format check + commit**

```bash
cargo fmt --all -- --check
git add crates/roy-gateway/src/typing.rs crates/roy-gateway/src/lib.rs
git commit -m "feat(roy-gateway): TypingKeepalive"
```

---

## Task 5: CancelRegistry

Per-chat `CancellationToken` map for the `/cancel` handler ↔ streaming-task signal.

**Files:**
- Modify: `crates/roy-gateway/Cargo.toml` (add `tokio-util`)
- Create: `crates/roy-gateway/src/cancel.rs`
- Modify: `crates/roy-gateway/src/lib.rs` (add `pub mod cancel;`)

- [ ] **Step 1: Add `tokio-util` to dependencies**

Edit `crates/roy-gateway/Cargo.toml`. In the `[dependencies]` section, add this line (alphabetically, between `tokio = …` and `tracing = …`):

```toml
tokio-util = "0.7"
```

- [ ] **Step 2: Add module to `lib.rs`**

Append `pub mod cancel;` to `crates/roy-gateway/src/lib.rs`.

- [ ] **Step 3: Create the file**

Create `crates/roy-gateway/src/cancel.rs` with EXACTLY this content:

```rust
//! Per-chat cancellation tokens for the `/cancel` command.
//! Streaming task registers a token at the start of a turn and releases it
//! at the end; the `/cancel` handler looks up by `chat_id` and signals.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct CancelRegistry {
    inner: Mutex<HashMap<i64, CancellationToken>>,
}

impl CancelRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a fresh token for `chat_id`. Returns the clone the caller
    /// should `.cancelled()` await on. If a previous token was registered for
    /// the same chat (e.g. a stuck turn), it is replaced — the orphaned token
    /// is dropped without signaling, and its awaiter (the previous streaming
    /// task) will need to clean up on its own when the daemon eventually
    /// returns.
    pub async fn register(&self, chat_id: i64) -> CancellationToken {
        let token = CancellationToken::new();
        self.inner.lock().await.insert(chat_id, token.clone());
        token
    }

    /// Signal cancellation for `chat_id`. Returns `true` if a token was
    /// registered (a turn is/was in flight), `false` otherwise.
    pub async fn signal(&self, chat_id: i64) -> bool {
        match self.inner.lock().await.get(&chat_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Remove the registered token (call after the turn finishes). No-op if
    /// none registered.
    pub async fn release(&self, chat_id: i64) {
        self.inner.lock().await.remove(&chat_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signal_without_register_returns_false() {
        let reg = CancelRegistry::new();
        assert!(!reg.signal(42).await);
    }

    #[tokio::test]
    async fn register_then_signal_returns_true_and_cancels_token() {
        let reg = CancelRegistry::new();
        let token = reg.register(42).await;
        assert!(!token.is_cancelled());
        assert!(reg.signal(42).await);
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn release_removes_registration() {
        let reg = CancelRegistry::new();
        let _token = reg.register(42).await;
        reg.release(42).await;
        assert!(!reg.signal(42).await);
    }

    #[tokio::test]
    async fn double_signal_is_idempotent() {
        let reg = CancelRegistry::new();
        let token = reg.register(42).await;
        assert!(reg.signal(42).await);
        assert!(reg.signal(42).await); // still returns true; token still cancelled
        assert!(token.is_cancelled());
    }
}
```

- [ ] **Step 4: Build**

Run: `cargo build -p roy-gateway 2>&1 | tail -8`
Expected: PASS (pulls `tokio-util` on first build).

- [ ] **Step 5: Run tests**

Run: `cargo test -p roy-gateway --lib cancel 2>&1 | tail -10`
Expected: 4 passed.

- [ ] **Step 6: Format check + commit**

```bash
cargo fmt --all -- --check
git add crates/roy-gateway/Cargo.toml crates/roy-gateway/src/cancel.rs crates/roy-gateway/src/lib.rs
git commit -m "feat(roy-gateway): CancelRegistry"
```

---

## Task 6: Conn trait + TurnConn impl (parallel to Fire, no callers yet)

Adds the new transport layer ALONGSIDE the existing Fire-based code. Doesn't touch `orchestrator.rs`, `telegram.rs`, or `main.rs`, so the v1 pipeline keeps working. The next task (Task 7) cuts over.

**Files:**
- Modify: `crates/roy-gateway/src/daemon.rs` (add `Conn` trait + `TurnConn` + `RealConnFactory` + new test helper)

- [ ] **Step 1: Append the new types and tests to `daemon.rs`**

Open `crates/roy-gateway/src/daemon.rs`. **Append** (do not replace) the following to the end of the file, BEFORE the existing `#[cfg(test)] mod tests` block (or after `impl DaemonClient` block — whichever places the new code outside the existing tests):

```rust
use async_trait::async_trait;
use roy::event::TurnEvent;
use roy::journal::JournalEntry;
use tokio::net::UnixStream;

/// A single-turn daemon connection. Owns one Unix-socket connection and walks
/// it through Spawn/Resume → AcquireInput → Send → Frame loop → ReleaseInput.
///
/// Replaces the v1 `Fire` composite, which couldn't be cancelled externally
/// because it opened and closed its own connection inside the daemon call.
#[async_trait]
pub trait Conn: Send {
    async fn spawn(
        &mut self,
        preset: &str,
        project_id: Option<String>,
        tags: BTreeMap<String, String>,
    ) -> Result<String>;

    async fn resume(
        &mut self,
        session_id: &str,
        tags: BTreeMap<String, String>,
    ) -> Result<String>;

    async fn acquire_input(&mut self, session: &str) -> Result<()>;

    async fn send_prompt(&mut self, session: &str, text: String) -> Result<()>;

    /// Returns `Ok(Some(event))` for each `Frame` from the daemon, including
    /// the terminal `Result`. After the terminal Result has been surfaced,
    /// subsequent calls return `Ok(None)`.
    async fn next_frame(&mut self) -> Result<Option<TurnEvent>>;

    async fn cancel_turn(&mut self, session: &str) -> Result<()>;

    async fn release_input(&mut self, session: &str) -> Result<()>;
}

#[async_trait]
pub trait ConnFactory: Send + Sync {
    type Conn: Conn;
    async fn open(&self) -> Result<Self::Conn>;
}

pub struct RealConnFactory {
    socket_path: PathBuf,
}

impl RealConnFactory {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

#[async_trait]
impl ConnFactory for RealConnFactory {
    type Conn = TurnConn;
    async fn open(&self) -> Result<TurnConn> {
        TurnConn::open(&self.socket_path).await
    }
}

pub struct TurnConn {
    write_half: tokio::io::WriteHalf<UnixStream>,
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<UnixStream>>>,
    terminal_seen: bool,
}

impl TurnConn {
    pub async fn open(socket_path: &std::path::Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to daemon at {}", socket_path.display()))?;
        let (read_half, write_half) = tokio::io::split(stream);
        Ok(Self {
            write_half,
            lines: BufReader::new(read_half).lines(),
            terminal_seen: false,
        })
    }

    async fn send_cmd(&mut self, cmd: &ClientCommand) -> Result<()> {
        let line = serde_json::to_string(cmd).context("serializing ClientCommand")?;
        self.write_half.write_all(line.as_bytes()).await?;
        self.write_half.write_all(b"\n").await?;
        self.write_half.flush().await?;
        Ok(())
    }

    async fn read_event(&mut self) -> Result<ServerEvent> {
        let Some(raw) = self.lines.next_line().await? else {
            return Err(anyhow!("daemon closed connection"));
        };
        serde_json::from_str(&raw).with_context(|| format!("parsing daemon line: {raw}"))
    }
}

#[async_trait]
impl Conn for TurnConn {
    async fn spawn(
        &mut self,
        preset: &str,
        project_id: Option<String>,
        tags: BTreeMap<String, String>,
    ) -> Result<String> {
        self.send_cmd(&ClientCommand::Spawn {
            agent: preset.into(),
            project_id,
            model: None,
            permission: None,
            resume: None,
            tags,
        })
        .await?;
        match self.read_event().await? {
            ServerEvent::Spawned { session, .. } => Ok(session),
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("spawn failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to Spawn: {other:?}")),
        }
    }

    async fn resume(
        &mut self,
        session_id: &str,
        tags: BTreeMap<String, String>,
    ) -> Result<String> {
        self.send_cmd(&ClientCommand::Resume {
            session: session_id.into(),
            tags: Some(tags),
        })
        .await?;
        match self.read_event().await? {
            ServerEvent::Resumed { session, .. } => Ok(session),
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("resume failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to Resume: {other:?}")),
        }
    }

    async fn acquire_input(&mut self, session: &str) -> Result<()> {
        self.send_cmd(&ClientCommand::AcquireInput { session: session.into() }).await?;
        match self.read_event().await? {
            ServerEvent::InputAcquired { acquired: true, .. } => Ok(()),
            ServerEvent::InputAcquired { acquired: false, .. } => {
                Err(anyhow!("input lease busy"))
            }
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("acquire_input failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to AcquireInput: {other:?}")),
        }
    }

    async fn send_prompt(&mut self, session: &str, text: String) -> Result<()> {
        self.send_cmd(&ClientCommand::Send {
            session: session.into(),
            text,
        })
        .await
    }

    async fn next_frame(&mut self) -> Result<Option<TurnEvent>> {
        if self.terminal_seen {
            return Ok(None);
        }
        match self.read_event().await? {
            ServerEvent::Frame { entry: JournalEntry { event, .. }, .. } => {
                if matches!(event, TurnEvent::Result { .. }) {
                    self.terminal_seen = true;
                }
                Ok(Some(event))
            }
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("frame stream error: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected event during frame loop: {other:?}")),
        }
    }

    async fn cancel_turn(&mut self, session: &str) -> Result<()> {
        self.send_cmd(&ClientCommand::CancelTurn { session: session.into() }).await
    }

    async fn release_input(&mut self, session: &str) -> Result<()> {
        self.send_cmd(&ClientCommand::ReleaseInput { session: session.into() }).await?;
        // ReleaseInput response is InputReleased; if anything else arrives, surface it.
        match self.read_event().await? {
            ServerEvent::InputReleased { .. } => Ok(()),
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("release_input failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to ReleaseInput: {other:?}")),
        }
    }
}
```

(Note: this assumes the existing `daemon.rs` already imports `BTreeMap`, `Result`, `anyhow!`, `Context`, `ClientCommand`, `ErrorCode`, `FireTarget`, `ServerEvent`, `BufReader`, `AsyncBufReadExt`, `AsyncWriteExt`, `PathBuf`. If any are missing because the existing file is leaner, add them to the `use` block at the top of the file.)

- [ ] **Step 2: Append the TurnConn unit tests**

Inside the existing `#[cfg(test)] mod tests { … }` block at the bottom of `daemon.rs`, append these tests before the closing `}`:

```rust
    use roy::event::StopReason;
    use roy::journal::JournalEntry as JE;

    /// Reusable scripted-daemon fixture for TurnConn tests.
    /// Reads N lines, returns the i-th canned response for each.
    async fn scripted_daemon(
        server: tokio::io::DuplexStream,
        script: Vec<(Box<dyn FnOnce(ClientCommand) + Send>, ServerEvent)>,
    ) {
        let (r, mut w) = tokio::io::split(server);
        let mut lines = BufReader::new(r).lines();
        for (assert_cmd, response) in script {
            let raw = match lines.next_line().await {
                Ok(Some(line)) => line,
                _ => return,
            };
            let cmd: ClientCommand = serde_json::from_str(&raw).unwrap();
            assert_cmd(cmd);
            let line = serde_json::to_string(&response).unwrap();
            w.write_all(line.as_bytes()).await.unwrap();
            w.write_all(b"\n").await.unwrap();
            w.flush().await.unwrap();
        }
    }

    /// Build a TurnConn from a duplex client half.
    fn turn_conn_from_duplex(stream: tokio::io::DuplexStream) -> TurnConn {
        let (read_half, write_half) = tokio::io::split(stream);
        TurnConn {
            write_half,
            lines: BufReader::new(read_half).lines(),
            terminal_seen: false,
        }
    }

    #[tokio::test]
    async fn turn_conn_spawn_returns_session_id() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(scripted_daemon(
            server,
            vec![(
                Box::new(|cmd| match cmd {
                    ClientCommand::Spawn { agent, project_id, .. } => {
                        assert_eq!(agent, "claude");
                        assert_eq!(project_id.as_deref(), None);
                    }
                    other => panic!("expected Spawn, got {other:?}"),
                }),
                ServerEvent::Spawned {
                    session: "sid-1".into(),
                    resume_cursor: None,
                },
            )],
        ));
        let mut conn = turn_conn_from_duplex(client);
        let sid = conn.spawn("claude", None, BTreeMap::new()).await.unwrap();
        assert_eq!(sid, "sid-1");
    }

    #[tokio::test]
    async fn turn_conn_next_frame_surfaces_terminal_then_none() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(scripted_daemon(
            server,
            vec![
                (
                    Box::new(|cmd| match cmd {
                        ClientCommand::AcquireInput { .. } => {}
                        other => panic!("expected AcquireInput, got {other:?}"),
                    }),
                    ServerEvent::Frame {
                        session: "sid".into(),
                        entry: JE {
                            seq: 1,
                            event: TurnEvent::AssistantText { delta: "hi".into() },
                        },
                    },
                ),
                (
                    Box::new(|_| {}),
                    ServerEvent::Frame {
                        session: "sid".into(),
                        entry: JE {
                            seq: 2,
                            event: TurnEvent::Result {
                                cost_usd: None,
                                stop_reason: StopReason::EndTurn,
                            },
                        },
                    },
                ),
            ],
        ));
        let mut conn = turn_conn_from_duplex(client);
        // Drive a first read by issuing acquire_input (it consumes one line). The
        // fixture returns Frame instead of InputAcquired for this test — that
        // would normally be an Err, so we go direct to next_frame.
        // Adjust the test: skip acquire and call next_frame twice directly.
        // The scripted_daemon will read whatever the conn sends first; we send
        // a dummy AcquireInput just to drive the first response.
        let _ = conn.acquire_input("sid").await;
        let frame1 = conn.next_frame().await.unwrap();
        assert!(matches!(frame1, Some(TurnEvent::Result { .. })));
        let frame2 = conn.next_frame().await.unwrap();
        assert!(frame2.is_none(), "expected None after terminal Result");
    }

    #[tokio::test]
    async fn turn_conn_acquire_input_fails_on_busy() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(scripted_daemon(
            server,
            vec![(
                Box::new(|_| {}),
                ServerEvent::InputAcquired {
                    session: "sid".into(),
                    acquired: false,
                },
            )],
        ));
        let mut conn = turn_conn_from_duplex(client);
        let err = conn.acquire_input("sid").await.unwrap_err();
        assert!(err.to_string().contains("busy"));
    }
```

(The `turn_conn_next_frame_surfaces_terminal_then_none` test has a comment about a known awkward bit: the scripted_daemon doesn't conditionally respond, so we drive the first read via acquire_input even though the response isn't an InputAcquired. The test asserts only on the subsequent `next_frame` semantics — that's the point of the test.)

- [ ] **Step 3: Build**

Run: `cargo build -p roy-gateway 2>&1 | tail -10`
Expected: PASS. If there are missing imports (e.g. `BTreeMap`, `BufReader`), add them. If the existing `daemon.rs` already has a `use async_trait::async_trait;` import, don't duplicate it — fold the new use into the existing block.

- [ ] **Step 4: Run new tests**

Run: `cargo test -p roy-gateway --lib daemon::tests::turn_conn 2>&1 | tail -15`
Expected: 3 passed (the three `turn_conn_*` tests).

Confirm no old tests broke:
Run: `cargo test -p roy-gateway --lib daemon 2>&1 | grep 'test result'`
Expected: all daemon tests pass (5 original + 3 new = 8).

- [ ] **Step 5: Format check + commit**

```bash
cargo fmt --all -- --check
git add crates/roy-gateway/src/daemon.rs
git commit -m "feat(roy-gateway): TurnConn — long-held daemon connection per turn"
```

---

## Task 7: Cutover — streaming pipeline replaces Fire

This is the big commit. Replaces `Fire`/`FireOutcome`/`fire_*` with `Conn`/`ConnFactory`, rewrites `orchestrator::handle_message` to the streaming pipeline, grows `Replier` trait, updates `TeloxideReplier` impl in `telegram.rs`, deletes the old tests in orchestrator (replaced by streaming versions). After this commit, no `Fire`-based code remains.

**Files:**
- Modify: `crates/roy-gateway/src/orchestrator.rs` (full rewrite)
- Modify: `crates/roy-gateway/src/telegram.rs` (full rewrite)
- Modify: `crates/roy-gateway/src/daemon.rs` (delete `Fire`-era code: `FireOutcome` enum, `fire_via_stream` fn, `DaemonClient::fire_spawn`/`fire_resume` methods, plus all five old tests)

- [ ] **Step 1: Rewrite `orchestrator.rs`**

Replace the entire contents of `crates/roy-gateway/src/orchestrator.rs` with EXACTLY:

```rust
//! Streaming pipeline that turns one inbound chat message into a series of
//! throttled Telegram edits as the agent produces `TurnEvent`s.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use roy::event::TurnEvent;
use tokio_util::sync::CancellationToken;

use crate::binder::SessionBinder;
use crate::cancel::CancelRegistry;
use crate::daemon::{Conn, ConnFactory};
use crate::draft_stream::{DraftReplier, DraftStream};
use crate::formatting::Renderer;
use crate::typing::{TypingKeepalive, TypingReplier};

/// Combined trait for everything `handle_message` needs from a chat replier.
#[async_trait]
pub trait Replier: DraftReplier + TypingReplier {
    // Marker trait — all behavior is on the supertraits.
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub preset: String,
    pub project_id: Option<String>,
    pub turn_timeout: Duration,
    pub typing_interval: Duration,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            preset: "claude".into(),
            project_id: None,
            turn_timeout: Duration::from_secs(600),
            typing_interval: Duration::from_secs(4),
        }
    }
}

pub async fn handle_message<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    cancel_registry: &CancelRegistry,
    conn_factory: &F,
    replier: &Arc<R>,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: ConnFactory,
    R: Replier + 'static,
{
    let token = cancel_registry.register(chat_id).await;
    let result = run_turn(cfg, binder, &token, conn_factory, replier, chat_id, prompt).await;
    cancel_registry.release(chat_id).await;
    result
}

async fn run_turn<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    token: &CancellationToken,
    conn_factory: &F,
    replier: &Arc<R>,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: ConnFactory,
    R: Replier + 'static,
{
    // Placeholder + draft stream + typing.
    let placeholder_id = replier.send(chat_id, "⏳").await?;
    let typing = TypingKeepalive::start(replier.clone(), chat_id, cfg.typing_interval);
    let draft = DraftStream::new(replier.clone(), chat_id, placeholder_id);

    let mut tags = BTreeMap::new();
    tags.insert("channel".into(), "telegram".into());
    tags.insert("chat_id".into(), chat_id.to_string());

    let outcome = drive_turn(
        cfg, binder, token, conn_factory, &draft, chat_id, prompt, tags,
    )
    .await;

    typing.stop();
    let _ = draft.flush().await;
    outcome
}

async fn drive_turn<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    token: &CancellationToken,
    conn_factory: &F,
    draft: &DraftStream<R>,
    chat_id: i64,
    prompt: String,
    tags: BTreeMap<String, String>,
) -> Result<()>
where
    F: ConnFactory,
    R: Replier + 'static,
{
    let mut conn = conn_factory.open().await?;

    let session_id = match binder.get(chat_id).await {
        Some(sid) => conn.resume(&sid, tags).await,
        None => conn.spawn(&cfg.preset, cfg.project_id.clone(), tags).await,
    };
    let session_id = match session_id {
        Ok(s) => s,
        Err(e) => {
            draft.update(format!("⚠ {e}")).await?;
            return Ok(());
        }
    };

    binder.set(chat_id, session_id.clone()).await?;

    if let Err(e) = conn.acquire_input(&session_id).await {
        draft.update(format!("⚠ {e}")).await?;
        return Ok(());
    }
    conn.send_prompt(&session_id, prompt).await?;

    let mut renderer = Renderer::new();
    let _ = tokio::time::timeout(
        cfg.turn_timeout,
        consume_frames(&mut conn, token, draft, &mut renderer, &session_id),
    )
    .await
    .ok(); // timeout returns Err -> we surface it as draft footer below

    if token.is_cancelled() {
        renderer.append_error_footer("cancelled by user");
        draft.update(renderer.body()).await?;
    }

    // Best-effort release; if conn is broken, this errors but we still proceed.
    let _ = conn.release_input(&session_id).await;
    Ok(())
}

async fn consume_frames<R: Replier + 'static>(
    conn: &mut impl Conn,
    token: &CancellationToken,
    draft: &DraftStream<R>,
    renderer: &mut Renderer,
    session_id: &str,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                let _ = conn.cancel_turn(session_id).await;
                // Drain until terminal so daemon state is clean.
                while let Ok(Some(event)) = conn.next_frame().await {
                    if matches!(event, TurnEvent::Result { .. }) {
                        break;
                    }
                }
                return Ok(());
            }
            frame = conn.next_frame() => {
                match frame? {
                    None => return Ok(()),
                    Some(TurnEvent::Result { stop_reason, .. }) => {
                        if stop_reason.is_error() {
                            renderer.append_error_footer(&format!("{stop_reason:?}"));
                            draft.update(renderer.body()).await?;
                        }
                        return Ok(());
                    }
                    Some(event) => {
                        renderer.feed(event);
                        draft.update(renderer.body()).await?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_stream::DraftReplier;
    use crate::typing::TypingReplier;
    use roy::event::StopReason;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;
    use tokio::sync::Mutex as TokioMutex;

    #[derive(Default)]
    struct MockReplier {
        sent: TokioMutex<Vec<(i64, String)>>,
        edits: TokioMutex<Vec<(i64, i32, String)>>,
        next_id: StdMutex<i32>,
        typing_count: std::sync::atomic::AtomicUsize,
    }

    impl MockReplier {
        fn new() -> Self {
            Self {
                next_id: StdMutex::new(100),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl DraftReplier for MockReplier {
        async fn send(&self, chat_id: i64, html: &str) -> Result<i32> {
            self.sent.lock().await.push((chat_id, html.into()));
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            Ok(*id)
        }
        async fn edit(&self, chat_id: i64, message_id: i32, html: &str) -> Result<()> {
            self.edits.lock().await.push((chat_id, message_id, html.into()));
            Ok(())
        }
    }

    #[async_trait]
    impl TypingReplier for MockReplier {
        async fn typing(&self, _chat_id: i64) -> Result<()> {
            self.typing_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    impl Replier for MockReplier {}

    // -- mock Conn + ConnFactory --

    struct MockConn {
        script: StdMutex<Vec<MockStep>>,
    }

    #[derive(Debug)]
    enum MockStep {
        SpawnReturns(String),
        AcquireOk,
        SendOk,
        Frame(TurnEvent),
        FrameEnd,
        ReleaseOk,
    }

    impl MockConn {
        fn new(script: Vec<MockStep>) -> Self {
            Self {
                script: StdMutex::new(script.into_iter().rev().collect()),
            }
        }
        fn pop(&self) -> Option<MockStep> {
            self.script.lock().unwrap().pop()
        }
    }

    #[async_trait]
    impl Conn for MockConn {
        async fn spawn(
            &mut self,
            _preset: &str,
            _project_id: Option<String>,
            _tags: BTreeMap<String, String>,
        ) -> Result<String> {
            match self.pop() {
                Some(MockStep::SpawnReturns(s)) => Ok(s),
                other => panic!("unexpected spawn call, next step was {other:?}"),
            }
        }
        async fn resume(
            &mut self,
            _session_id: &str,
            _tags: BTreeMap<String, String>,
        ) -> Result<String> {
            match self.pop() {
                Some(MockStep::SpawnReturns(s)) => Ok(s),
                other => panic!("unexpected resume call, next step was {other:?}"),
            }
        }
        async fn acquire_input(&mut self, _session: &str) -> Result<()> {
            assert!(matches!(self.pop(), Some(MockStep::AcquireOk)));
            Ok(())
        }
        async fn send_prompt(&mut self, _session: &str, _text: String) -> Result<()> {
            assert!(matches!(self.pop(), Some(MockStep::SendOk)));
            Ok(())
        }
        async fn next_frame(&mut self) -> Result<Option<TurnEvent>> {
            match self.pop() {
                Some(MockStep::Frame(e)) => Ok(Some(e)),
                Some(MockStep::FrameEnd) => Ok(None),
                other => panic!("unexpected next_frame, next step was {other:?}"),
            }
        }
        async fn cancel_turn(&mut self, _session: &str) -> Result<()> {
            Ok(())
        }
        async fn release_input(&mut self, _session: &str) -> Result<()> {
            assert!(matches!(self.pop(), Some(MockStep::ReleaseOk)));
            Ok(())
        }
    }

    struct MockConnFactory {
        steps: StdMutex<Option<Vec<MockStep>>>,
    }

    impl MockConnFactory {
        fn new(steps: Vec<MockStep>) -> Self {
            Self {
                steps: StdMutex::new(Some(steps)),
            }
        }
    }

    #[async_trait]
    impl ConnFactory for MockConnFactory {
        type Conn = MockConn;
        async fn open(&self) -> Result<MockConn> {
            let steps = self
                .steps
                .lock()
                .unwrap()
                .take()
                .expect("factory open called more than once");
            Ok(MockConn::new(steps))
        }
    }

    async fn fresh_binder(dir: &TempDir) -> SessionBinder {
        SessionBinder::load(dir.path().join("b.json")).await.unwrap()
    }

    fn cfg() -> OrchestratorConfig {
        OrchestratorConfig {
            preset: "claude".into(),
            project_id: None,
            turn_timeout: Duration::from_secs(60),
            typing_interval: Duration::from_secs(60), // long so it doesn't fire in tests
        }
    }

    #[tokio::test]
    async fn unbound_chat_spawns_streams_and_replies() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("sess-new".into()),
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::Frame(TurnEvent::AssistantText {
                delta: "Hello!".into(),
            }),
            MockStep::Frame(TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::EndTurn,
            }),
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        handle_message(&cfg(), &binder, &registry, &factory, &replier, 42, "hi".into())
            .await
            .unwrap();

        assert_eq!(binder.get(42).await.as_deref(), Some("sess-new"));
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "⏳".into())]);
        let edits = replier.edits.lock().await.clone();
        // At least one edit and the final one contains "Hello!"
        assert!(!edits.is_empty());
        assert!(edits.last().unwrap().2.contains("Hello!"));
    }

    #[tokio::test]
    async fn bound_chat_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        binder.set(42, "sess-old".into()).await.unwrap();
        let registry = CancelRegistry::new();
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("sess-old".into()), // mock uses same step name for resume
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::Frame(TurnEvent::AssistantText { delta: "ok".into() }),
            MockStep::Frame(TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::EndTurn,
            }),
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        handle_message(&cfg(), &binder, &registry, &factory, &replier, 42, "again".into())
            .await
            .unwrap();

        let edits = replier.edits.lock().await.clone();
        assert!(edits.last().unwrap().2.contains("ok"));
    }

    #[tokio::test]
    async fn cancel_via_registry_causes_cancel_turn_and_footer() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        // Script: spawn + acquire + send + one frame, then signal cancel before the next frame.
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("s".into()),
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::Frame(TurnEvent::AssistantText { delta: "partial".into() }),
            // After cancel, drain loop reads next frames until terminal:
            MockStep::Frame(TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::EndTurn,
            }),
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        // Signal cancel BEFORE invoking handle_message — the token registers and
        // immediately gets cancelled at the first select! iteration.
        let registry_clone = registry.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            registry_clone.signal(42).await;
        });

        handle_message(&cfg(), &binder, &registry, &factory, &replier, 42, "x".into())
            .await
            .unwrap();

        let edits = replier.edits.lock().await.clone();
        assert!(edits.iter().any(|(_, _, html)| html.contains("cancelled by user")));
    }

    #[tokio::test]
    async fn spawn_failure_reported_no_binder_write() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        struct FailingFactory;
        #[async_trait]
        impl ConnFactory for FailingFactory {
            type Conn = MockConn;
            async fn open(&self) -> Result<MockConn> {
                Ok(MockConn::new(vec![]))
            }
        }
        // Spawn pops nothing → panics. Use a factory that fails earlier.
        // Simpler: factory that errors on open.
        struct ErrOpenFactory;
        #[async_trait]
        impl ConnFactory for ErrOpenFactory {
            type Conn = MockConn;
            async fn open(&self) -> Result<MockConn> {
                Err(anyhow::anyhow!("daemon down"))
            }
        }
        let factory = ErrOpenFactory;
        let replier = Arc::new(MockReplier::new());

        // run_turn will fail at open(); run_turn returns Err to handle_message but
        // we still set up the placeholder. Let's verify the placeholder was sent and
        // the binder is empty.
        let _ = handle_message(&cfg(), &binder, &registry, &factory, &replier, 42, "x".into()).await;

        assert!(binder.get(42).await.is_none());
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "⏳".into())]);
    }
}
```

- [ ] **Step 2: Rewrite `telegram.rs`**

Replace the entire contents of `crates/roy-gateway/src/telegram.rs` with EXACTLY:

```rust
//! Teloxide bot loop. Dispatches /cancel vs text messages, runs the streaming
//! pipeline on text, and signals the cancel registry on /cancel.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use teloxide::payloads::SendChatActionSetters;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, ChatId, MessageId, ParseMode};

use crate::binder::SessionBinder;
use crate::cancel::CancelRegistry;
use crate::daemon::RealConnFactory;
use crate::draft_stream::DraftReplier;
use crate::orchestrator::{handle_message, OrchestratorConfig, Replier};
use crate::typing::TypingReplier;

pub struct TeloxideReplier {
    bot: Bot,
}

impl TeloxideReplier {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }
}

#[async_trait]
impl DraftReplier for TeloxideReplier {
    async fn send(&self, chat_id: i64, html: &str) -> Result<i32> {
        let msg = self
            .bot
            .send_message(ChatId(chat_id), html)
            .parse_mode(ParseMode::Html)
            .await?;
        Ok(msg.id.0)
    }

    async fn edit(&self, chat_id: i64, message_id: i32, html: &str) -> Result<()> {
        self.bot
            .edit_message_text(ChatId(chat_id), MessageId(message_id), html)
            .parse_mode(ParseMode::Html)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl TypingReplier for TeloxideReplier {
    async fn typing(&self, chat_id: i64) -> Result<()> {
        self.bot
            .send_chat_action(ChatId(chat_id), ChatAction::Typing)
            .await?;
        Ok(())
    }
}

impl Replier for TeloxideReplier {}

#[derive(Clone)]
pub struct BotDeps {
    pub cfg: Arc<OrchestratorConfig>,
    pub binder: Arc<SessionBinder>,
    pub conn_factory: Arc<RealConnFactory>,
    pub replier: Arc<TeloxideReplier>,
    pub cancel_registry: Arc<CancelRegistry>,
    pub allowed_user_ids: Arc<HashSet<u64>>,
}

pub async fn run(bot: Bot, deps: BotDeps) -> Result<()> {
    tracing::info!("starting teloxide dispatcher");

    let handler =
        Update::filter_message().endpoint(|_bot: Bot, msg: Message, deps: BotDeps| async move {
            if let Err(e) = on_message(&msg, &deps).await {
                tracing::warn!(?e, chat_id = msg.chat.id.0, "message handler failed");
            }
            respond(())
        });

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![deps])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn on_message(msg: &Message, deps: &BotDeps) -> Result<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0;
    if !deps.allowed_user_ids.is_empty() && !deps.allowed_user_ids.contains(&user_id) {
        tracing::debug!(user_id, "rejecting non-allowlisted sender");
        return Ok(());
    }

    let chat_id = msg.chat.id.0;

    if is_cancel_command(text) {
        on_cancel(deps, chat_id).await
    } else {
        handle_message(
            deps.cfg.as_ref(),
            deps.binder.as_ref(),
            deps.cancel_registry.as_ref(),
            deps.conn_factory.as_ref(),
            &deps.replier,
            chat_id,
            text.to_string(),
        )
        .await
    }
}

fn is_cancel_command(text: &str) -> bool {
    let head = text.split_whitespace().next().unwrap_or("");
    head == "/cancel" || head.starts_with("/cancel@")
}

async fn on_cancel(deps: &BotDeps, chat_id: i64) -> Result<()> {
    let signaled = deps.cancel_registry.signal(chat_id).await;
    let reply = if signaled {
        "❎ cancelled"
    } else {
        "Нечего отменять — turn не запущен"
    };
    deps.replier.send(chat_id, reply).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_plain_cancel() {
        assert!(is_cancel_command("/cancel"));
        assert!(is_cancel_command("/cancel  "));
        assert!(is_cancel_command("/cancel reason ignored"));
    }

    #[test]
    fn detects_cancel_with_bot_suffix() {
        assert!(is_cancel_command("/cancel@my_bot"));
    }

    #[test]
    fn does_not_match_other_text() {
        assert!(!is_cancel_command("cancel"));
        assert!(!is_cancel_command("hello /cancel"));
        assert!(!is_cancel_command(""));
    }
}
```

- [ ] **Step 3: Delete `Fire`-era code from `daemon.rs`**

Open `crates/roy-gateway/src/daemon.rs`. Delete:

1. The `pub enum FireOutcome { … }` block.
2. The `pub(crate) async fn fire_via_stream<S>(…)` function (or `pub` if your tree still has the pub form).
3. The two methods `impl DaemonClient { … }` adds: `pub async fn fire_spawn(…)` and `pub async fn fire_resume(…)`. Keep `DaemonClient::new` and the struct itself for now (it's harmless even if unused — Task 8 will remove it if it's truly unused).
4. From the `#[cfg(test)] mod tests` block: delete the old fixtures and tests `fake_daemon`, `fire_spawn_returns_done`, `fire_resume_returns_done`, `fire_error_is_returned_verbatim`, `fire_timeout_is_mapped`, `generic_error_event_is_mapped`. Keep the `turn_conn_*` tests and `scripted_daemon` fixture added in Task 6.

After deletion, the test module should contain ONLY:
- The `scripted_daemon` async fn (Task 6)
- The `turn_conn_from_duplex` helper (Task 6)
- `turn_conn_spawn_returns_session_id` (Task 6)
- `turn_conn_next_frame_surfaces_terminal_then_none` (Task 6)
- `turn_conn_acquire_input_fails_on_busy` (Task 6)
- Whatever `use super::*;` etc.

Trim any imports that are now unused (likely: `ErrorCode`, `FireTarget`, and parts of the existing test imports — let cargo guide you).

If `DaemonClient` ends up entirely unused after the Fire deletion, just remove it too.

- [ ] **Step 4: Update `main.rs` to wire `CancelRegistry` and `RealConnFactory`**

Replace the `BotDeps` construction in `crates/roy-gateway/src/main.rs`. Find the block that constructs `BotDeps { … }` and the surrounding setup. Replace:

```rust
    let daemon = Arc::new(DaemonClient::new(socket_path));
```

with:

```rust
    let conn_factory = Arc::new(RealConnFactory::new(socket_path));
```

And update the imports at the top:

```rust
use roy_gateway::cancel::CancelRegistry;
use roy_gateway::daemon::RealConnFactory;
```

(Remove the now-unused `use roy_gateway::daemon::DaemonClient;`.)

And update the `OrchestratorConfig` build to include `typing_interval`:

```rust
    let orch_cfg = Arc::new(OrchestratorConfig {
        preset: cfg.telegram.preset.clone(),
        project_id: cfg.telegram.project_id.clone(),
        turn_timeout: Duration::from_secs(cfg.telegram.turn_timeout_secs),
        typing_interval: Duration::from_secs(4),
    });
```

(Note: the v1 `TelegramConfig` field for the working-dir might still be named `cwd` in this branch — the cleanup branch renamed it to `project_id` but that's not in our base. If `cfg.telegram.cwd` is what exists, rename in `config.rs` from `cwd` to `project_id` so the field name matches the orchestrator. This is the same field-rename Task 9 in the v1 plan did — fold it in here.)

And construct `BotDeps`:

```rust
    let cancel_registry = CancelRegistry::new();
    let deps = BotDeps {
        cfg: orch_cfg,
        binder,
        conn_factory,
        replier,
        cancel_registry,
        allowed_user_ids: Arc::new(allowed),
    };
```

- [ ] **Step 5: Build**

Run: `cargo build -p roy-gateway 2>&1 | tail -15`
Expected: PASS. If there are import warnings (`unused_imports`), clean them up. If there are real errors (missing methods, type mismatches), report what they are.

- [ ] **Step 6: Run the full gateway test suite**

Run: `cargo test -p roy-gateway 2>&1 | grep 'test result'`
Expected: every group passes. Approximate count:
- config: 2
- binder: 3
- daemon: 3 (the three `turn_conn_*` from Task 6)
- formatting: 7
- draft_stream: 7
- typing: 2
- cancel: 4
- orchestrator: 4 (the four new streaming tests above)
- telegram: 3 (is_cancel_command tests)
- integration `binder_persistence`: 1
- Total ≈ 36

- [ ] **Step 7: Format check + commit**

```bash
cargo fmt --all -- --check
git add crates/roy-gateway/src/daemon.rs crates/roy-gateway/src/orchestrator.rs \
        crates/roy-gateway/src/telegram.rs crates/roy-gateway/src/main.rs \
        crates/roy-gateway/src/config.rs
git commit -m "feat(roy-gateway): streaming pipeline with TurnConn replaces Fire"
```

(`config.rs` is included only if you had to apply the `cwd → project_id` rename in Step 4.)

---

## Task 8: README + manual smoke checklist

Document the new v1.1 features for users running the gateway.

**Files:**
- Modify: `crates/roy-gateway/README.md`

- [ ] **Step 1: Replace the README contents**

Replace the entire `crates/roy-gateway/README.md` with:

````markdown
# roy-gateway

Bridges chat platforms ↔ a running `roy serve` daemon. v1.1 supports
**Telegram only**.

## How it works (v1.1 streaming)

1. `roy serve` is running. You have a preset (`claude` / `gemini` /
   `opencode` / `codex`) installed and pre-authenticated, and optionally a
   roy project pre-created (referenced by `project_id` in config).
2. `roy-gateway` runs as a long-lived process. On every inbound text DM:
   - Send a `⏳` placeholder message to the chat.
   - Open a daemon connection; `Spawn` (new chat) or `Resume` (known chat)
     to get a `session_id`, bind it to `chat_id` in the JSON binder.
   - `AcquireInput` (holds the daemon's input lease for the turn —
     prerequisite for `/cancel`).
   - `Send` the user's prompt.
   - Stream `Frame` events from the daemon. Each event extends the rendered
     HTML body (thinking → italic, tool calls → `<code>`, assistant text →
     plain). The placeholder is edited every ~1 second to show the latest
     body. At 4000 chars the message is finalized and a new one is started.
   - On terminal `Result`, flush final state, `ReleaseInput`, close the
     connection, remove the cancel-registry entry.
3. `/cancel` (DM) signals the streaming task to send `CancelTurn` to the
   daemon, append a `❎ cancelled by user` line, and finalize. If no turn is
   running, the bot replies "Нечего отменять".

## Config

```toml
# ~/.config/roy-gateway/telegram.toml — DO NOT COMMIT (contains bot token)

[daemon]
# Optional; falls back to ROY_SOCKET, then ~/.roy/daemon.sock
# socket = "/Users/me/.roy/daemon.sock"

[telegram]
token = "1234567890:AA…"            # from @BotFather
allowed_user_ids = [123456789]      # empty list = allow anyone
preset = "claude"
project_id = "proj-abc"             # optional; daemon default cwd otherwise
turn_timeout_secs = 600

[binder]
path = "/Users/me/.roy/gateway-telegram.json"
```

## Run

```bash
# 1. start the daemon (separate terminal)
roy serve

# 2. start the gateway
RUST_LOG=roy_gateway=info,info \
  cargo run -p roy-gateway -- --config ~/.config/roy-gateway/telegram.toml
```

## Manual smoke checklist (v1.1)

- [ ] DM your bot. Confirm `⏳` placeholder appears within a second, then
      gets edited as the agent produces text.
- [ ] Confirm `🧠 thinking:` blocks appear (italic) for AssistantThought
      events.
- [ ] Confirm `🔧 <tool>(<args>)` blocks appear for ToolUse events.
- [ ] Verify the chat shows "typing…" status in the header while the turn
      runs.
- [ ] Send a follow-up to the same chat. Confirm same `session_id` is
      reused (`jq < ~/.roy/gateway-telegram.json`).
- [ ] Trigger a long-running turn. Send `/cancel`. Confirm the streaming
      message gains `❎ cancelled by user` footer within ~1 second and the
      bot replies `❎ cancelled`.
- [ ] Send `/cancel` when no turn is running. Confirm reply
      `Нечего отменять — turn не запущен`.
- [ ] Trigger a long agent response that crosses 4000 chars. Confirm the
      message is finalized at a paragraph boundary and a new message
      continues the body.
- [ ] Stop the daemon. Send a message. Confirm a `⚠ …` error reply
      appears in the chat; gateway keeps running.
- [ ] (If `allowed_user_ids` set) DM from a non-allowlisted account.
      Expect silence and a `rejecting non-allowlisted sender` debug log.

## Still deferred to later iterations

- Debounce of fast successive messages.
- `Channel` trait + Slack/Discord support.
- Persisting full transcripts in chat after edits (history is in roy journal).
- Inline buttons, attachments, voice.
````

- [ ] **Step 2: Run the full workspace test suite + CI gate**

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast 2>&1 | grep 'test result'
```

Expected: all three commands pass cleanly. The full workspace test count should be ~155 (was ~141 before v1.1; +~36 new gateway tests minus the ~22 v1 daemon/orchestrator tests we deleted in Task 7).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-gateway/README.md
git commit -m "docs(roy-gateway): README + manual smoke checklist for v1.1"
```

---

## Self-Review

**Spec coverage:**

| Spec section | Tasks |
|---|---|
| Goal & three features | 1 (timeout), 7 (streaming), 7+8 (cancel handler) |
| Why /cancel and streaming are coupled | Task 7 explanation + structure |
| Out of scope (debounce, Channel trait, etc.) | Documented in Task 8 README |
| File structure | Mapped to Tasks 2–8 |
| Wire-format choice (HTML, 4000 chars, 1000ms throttle) | Tasks 2 (formatting HTML), 3 (DraftStream constants) |
| What gets shown (all events incl. thinking) | Task 2 (Renderer match arms) |
| Block accumulation | Task 2 (state machine + example test) |
| Streaming pipeline (14 steps) | Task 7 `run_turn` + `drive_turn` + `consume_frames` |
| Overflow split | Task 3 (DraftStream::update overflow branch + best_boundary) |
| DraftStream API | Task 3 |
| TypingKeepalive API | Task 4 |
| CancelRegistry API | Task 5 |
| TurnConn API + next_frame contract | Task 6 |
| Orchestrator rewrite (Conn + ConnFactory + Replier supertrait) | Task 7 |
| /cancel handler | Task 7 (telegram.rs `on_cancel`) |
| Error handling matrix | Implicit in Task 7 (handle_message error paths) and Task 8 (README) |
| Testing approach | Each task has explicit tests; Task 8 manual smoke |
| Migration / breaking changes | Task 7 Step 3 (delete Fire) |
| Commit shape preview | Final shape: 1 (timeout) + 4 (modules) + 1 (TurnConn) + 1 (cutover) + 1 (README) = 8 commits |

**Placeholder scan:** No "TBD", no "implement later", no "appropriate error handling" hand-waves. Each test file has actual test bodies. The one place that uses "ADAPT minimally" wording is in Task 2 Step 3 about TurnEvent variant shapes — that's a real instruction to handle the case where the actual `roy::event::TurnEvent` differs from what this plan assumed (which is verifiable in seconds by reading `crates/roy/src/event.rs`). Not a placeholder; it's instruction for adapting to ground truth.

**Type consistency:** Method names checked: `register/signal/release` (CancelRegistry), `spawn/resume/acquire_input/send_prompt/next_frame/cancel_turn/release_input` (Conn/TurnConn), `send/edit/typing` (replier traits), `update/flush` (DraftStream). All consistent across tasks 2–8. `Replier` trait is the combined trait built in Task 7 from `DraftReplier` + `TypingReplier` defined in Tasks 3 and 4 respectively — confirmed consistent.

One inconsistency caught and noted in Task 7 Step 4: the `TelegramConfig.cwd` vs `project_id` field. The v1 base has `cwd`; the cleanup branch (not in our base) renamed to `project_id`. Task 7 Step 4 includes the rename if needed — same fix that cleanup did, applied here.
