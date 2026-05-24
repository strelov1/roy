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
        assert!(
            count >= 3,
            "loop should keep ticking despite errors, got {count}"
        );
    }
}
