//! Outbound replies for the Telegram channel. The publisher pushes events with
//! `ReplyHandle::Noop`; the reply goes out-of-band through the bot via `TgSender`.

use anyhow::Result;
use async_trait::async_trait;

use crate::bus::ReplyHandle;
use crate::reply::{FireOutcome, ReplyHook};
use roy_protocol::TurnEvent;

/// Minimal send abstraction so the hook is unit-testable without a live bot.
#[async_trait]
pub trait TgSender: Send + Sync {
    async fn send(&self, chat_id: i64, text: &str) -> Result<()>;
}

/// Used when a bot was removed between event ingress and reply (rare race).
pub struct NoopSender;

#[async_trait]
impl TgSender for NoopSender {
    async fn send(&self, _chat_id: i64, _text: &str) -> Result<()> {
        tracing::warn!("telegram reply dropped: no live bot for source");
        Ok(())
    }
}

pub struct TelegramReplyHook {
    sender: std::sync::Arc<dyn TgSender>,
    chat_id: i64,
}

impl TelegramReplyHook {
    pub fn new(sender: std::sync::Arc<dyn TgSender>, chat_id: i64) -> Self {
        Self { sender, chat_id }
    }
}

#[async_trait]
impl ReplyHook for TelegramReplyHook {
    async fn on_turn_event(&mut self, _ev: &TurnEvent) -> Result<()> {
        Ok(()) // streaming edits are slice 4
    }

    async fn on_finish(self: Box<Self>, outcome: FireOutcome, _reply: ReplyHandle) -> Result<()> {
        let text = match outcome {
            FireOutcome::Ok { assistant_text, .. } => {
                if assistant_text.trim().is_empty() {
                    "(пустой ответ)".to_string()
                } else {
                    assistant_text
                }
            }
            FireOutcome::Timeout { .. } => "⚠ Превышено время ожидания, попробуйте ещё раз.".into(),
            FireOutcome::DaemonError { .. } => "⚠ Внутренняя ошибка, попробуйте позже.".into(),
            FireOutcome::Cancelled => "⚠ Запрос отменён.".into(),
            FireOutcome::RouteRejected => "⚠ Этот бот пока не настроен.".into(),
        };
        self.sender.send(self.chat_id, &text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockSender {
        sent: Mutex<Vec<(i64, String)>>,
    }
    #[async_trait]
    impl TgSender for MockSender {
        async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
            self.sent.lock().unwrap().push((chat_id, text.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn ok_outcome_sends_assistant_text() {
        let sender = Arc::new(MockSender::default());
        let hook = Box::new(TelegramReplyHook::new(sender.clone(), 555));
        hook.on_finish(
            FireOutcome::Ok {
                assistant_text: "hello".into(),
                cost_usd: None,
                stop_reason: "end_turn".into(),
            },
            ReplyHandle::Noop,
        )
        .await
        .unwrap();
        let sent = sender.sent.lock().unwrap();
        assert_eq!(sent.as_slice(), &[(555, "hello".to_string())]);
    }

    #[tokio::test]
    async fn error_outcome_sends_friendly_message() {
        let sender = Arc::new(MockSender::default());
        let hook = Box::new(TelegramReplyHook::new(sender.clone(), 7));
        hook.on_finish(FireOutcome::RouteRejected, ReplyHandle::Noop)
            .await
            .unwrap();
        assert!(sender.sent.lock().unwrap()[0].1.contains("не настроен"));
    }
}
