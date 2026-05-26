//! Webhook reply hook. Receives the fire outcome, encodes it as JSON, and
//! sends it through the `ReplyHandle::HttpSync` oneshot. If the handle is
//! `Noop` (async mode) the outcome is just logged and dropped.

use anyhow::Result;
use async_trait::async_trait;
use axum::http::StatusCode;
use roy::event::TurnEvent;
use serde_json::json;

use crate::bus::{HttpReply, ReplyHandle};
use crate::reply::{FireOutcome, ReplyHook};

pub struct WebhookReplyHook {
    event_id: String,
}

impl WebhookReplyHook {
    pub fn new(event_id: String) -> Self {
        Self { event_id }
    }
}

#[async_trait]
impl ReplyHook for WebhookReplyHook {
    async fn on_turn_event(&mut self, _ev: &TurnEvent) -> Result<()> {
        Ok(())
    }

    async fn on_finish(self: Box<Self>, outcome: FireOutcome, reply: ReplyHandle) -> Result<()> {
        let (status, body) = match outcome {
            FireOutcome::Ok {
                assistant_text,
                cost_usd,
                stop_reason,
            } => (
                StatusCode::OK,
                json!({
                    "ok": true,
                    "event_id": self.event_id,
                    "assistant_text": assistant_text,
                    "cost_usd": cost_usd,
                    "stop_reason": stop_reason,
                })
                .to_string(),
            ),
            FireOutcome::RouteRejected => (
                StatusCode::NOT_FOUND,
                json!({"ok": false, "error": "route_rejected"}).to_string(),
            ),
            FireOutcome::Timeout { .. } => (
                StatusCode::GATEWAY_TIMEOUT,
                json!({"ok": false, "error": "timeout"}).to_string(),
            ),
            FireOutcome::Cancelled => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": "cancelled"}).to_string(),
            ),
            FireOutcome::DaemonError { code, message } => (
                StatusCode::BAD_GATEWAY,
                json!({"ok": false, "error": "daemon", "code": code.to_string(),
                       "message": message})
                .to_string(),
            ),
        };

        match reply {
            ReplyHandle::Noop => {
                tracing::info!(
                    event_id = self.event_id,
                    %status,
                    "webhook reply (async mode): dropping outcome"
                );
            }
            ReplyHandle::HttpSync(tx) => {
                let _ = tx.send(HttpReply { status, body });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn ok_outcome_sends_200_with_body() {
        let (tx, rx) = oneshot::channel();
        let hook = Box::new(WebhookReplyHook::new("evt-1".into()));
        hook.on_finish(
            FireOutcome::Ok {
                assistant_text: "hi".into(),
                cost_usd: Some(0.01),
                stop_reason: "EndTurn".into(),
            },
            ReplyHandle::HttpSync(tx),
        )
        .await
        .unwrap();
        let r = rx.await.unwrap();
        assert_eq!(r.status, StatusCode::OK);
        assert!(r.body.contains("\"assistant_text\":\"hi\""));
    }

    #[tokio::test]
    async fn route_rejected_sends_404() {
        let (tx, rx) = oneshot::channel();
        let hook = Box::new(WebhookReplyHook::new("evt-1".into()));
        hook.on_finish(FireOutcome::RouteRejected, ReplyHandle::HttpSync(tx))
            .await
            .unwrap();
        let r = rx.await.unwrap();
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn noop_handle_just_logs() {
        let hook = Box::new(WebhookReplyHook::new("evt-1".into()));
        hook.on_finish(
            FireOutcome::Ok {
                assistant_text: "hi".into(),
                cost_usd: None,
                stop_reason: "EndTurn".into(),
            },
            ReplyHandle::Noop,
        )
        .await
        .unwrap();
    }
}
