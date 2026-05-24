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
