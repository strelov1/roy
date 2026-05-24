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
    dirty: bool, // true if current_body differs from what was last sent
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
                dirty: false,
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
            self.replier
                .edit(self.chat_id, guard.current_id, head)
                .await?;
            // Start a new message with the tail as its initial body.
            let new_id = self.replier.send(self.chat_id, tail).await?;
            guard.current_id = new_id;
            guard.current_body = tail.to_string();
            guard.last_sent_at = Instant::now();
            guard.dirty = false;
            return Ok(());
        }

        if full_body == guard.current_body {
            return Ok(());
        }

        if guard.last_sent_at.elapsed() < self.throttle {
            guard.current_body = full_body;
            guard.dirty = true;
            return Ok(());
        }

        self.replier
            .edit(self.chat_id, guard.current_id, &full_body)
            .await?;
        guard.current_body = full_body;
        guard.last_sent_at = Instant::now();
        guard.dirty = false;
        Ok(())
    }

    /// Force the latest body to be written even if we're inside the throttle window.
    /// Use at end-of-turn to make sure the final state is visible.
    pub async fn flush(&self) -> Result<()> {
        let mut guard = self.state.lock().await;
        if guard.current_body.is_empty() || !guard.dirty {
            return Ok(());
        }
        self.replier
            .edit(self.chat_id, guard.current_id, &guard.current_body)
            .await?;
        guard.dirty = false;
        Ok(())
    }
}

fn best_boundary(text: &str, max: usize) -> usize {
    if text.len() <= max {
        return text.len();
    }
    // Find the largest char boundary <= max; from there, prefer paragraph,
    // then line, then word boundaries inside the safe head.
    let max_char_boundary = text.floor_char_boundary(max);
    let head = &text[..max_char_boundary];
    if let Some(idx) = head.rfind("\n\n") {
        return idx + 2;
    }
    if let Some(idx) = head.rfind('\n') {
        return idx + 1;
    }
    if let Some(idx) = head.rfind(' ') {
        return idx + 1;
    }
    max_char_boundary
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
            self.edits
                .lock()
                .unwrap()
                .push((chat_id, message_id, html.into()));
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
        let stream =
            DraftStream::with_throttle(replier.clone(), 7, 100, Duration::from_millis(500));
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
        let stream =
            DraftStream::with_throttle(replier.clone(), 7, 100, Duration::from_millis(500));
        stream.update("one".into()).await.unwrap();
        stream.update("two".into()).await.unwrap();
        stream.flush().await.unwrap();
        let edits = replier.edits.lock().unwrap().clone();
        // First edit was "one"; flush forces "two" through.
        assert_eq!(edits.last().unwrap().2, "two");
    }

    #[tokio::test]
    async fn flush_skips_when_nothing_changed_since_last_send() {
        let replier = Arc::new(MockReplier::new(100));
        let stream = DraftStream::new(replier.clone(), 7, 100);
        stream.update("hello".into()).await.unwrap(); // sends an edit
        stream.flush().await.unwrap(); // should NOT send another
        let edits = replier.edits.lock().unwrap().clone();
        assert_eq!(edits.len(), 1, "flush after clean update must not re-edit");
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

    #[test]
    fn best_boundary_handles_multi_byte_chars_at_max() {
        // 'я' is 2 bytes in UTF-8. Build a string where byte 10 lands mid-char.
        let text = "ababababaяbcdef"; // ab × 4.5 = 9 ASCII bytes, then я at byte 9..11
                                      // max = 10 lands in the middle of я (bytes 9–10).
        let idx = best_boundary(text, 10);
        // Must return a valid char boundary: either 9 (before я) or 11 (after я).
        assert!(text.is_char_boundary(idx));
        assert!(idx <= 10); // must not exceed the requested cap
    }

    #[test]
    fn best_boundary_handles_long_cyrillic_body() {
        // 4000+ bytes of Cyrillic with no word breaks.
        let body = "Привет".repeat(500); // each "Привет" is 12 bytes; 500*12 = 6000 bytes
        let idx = best_boundary(&body, 4000);
        // Must be a valid char boundary <= 4000.
        assert!(body.is_char_boundary(idx));
        assert!(idx <= 4000);
    }
}
