//! Bus payload types. `InboundEvent` is what publishers push and the
//! dispatcher consumes. `ReplyHandle` is the typed token carried on the
//! event that tells the per-channel ReplyHook how to deliver back.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Response, StatusCode};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub type BusSender = mpsc::Sender<InboundEvent>;
pub type BusReceiver = mpsc::Receiver<InboundEvent>;

pub fn channel(capacity: usize) -> (BusSender, BusReceiver) {
    mpsc::channel(capacity)
}

#[derive(Debug)]
pub struct InboundEvent {
    pub id: Uuid,
    pub source_id: String,
    pub source_kind: String,
    pub sender_id: String,
    pub payload: Value,
    pub received_at: DateTime<Utc>,
    pub reply: ReplyHandle,
}

#[derive(Debug)]
pub enum ReplyHandle {
    Noop,
    HttpSync(oneshot::Sender<HttpReply>),
}

impl ReplyHandle {
    pub fn is_noop(&self) -> bool {
        matches!(self, Self::Noop)
    }
}

#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: StatusCode,
    pub body: String,
}

impl HttpReply {
    pub fn into_response(self) -> Response<Body> {
        Response::builder()
            .status(self.status)
            .header("content-type", "application/json")
            .body(Body::from(self.body))
            .unwrap_or_else(|_| Response::new(Body::empty()))
    }
}

/// Marker used in tags maps to identify roy-inbound dispatches.
pub const TAG_PREFIX: &str = "roy-inbound";

/// Helper newtype so non-event consumers (router, hook factories) can be
/// cloned without cloning the oneshot sender.
#[derive(Debug, Clone)]
pub struct EventRef {
    pub id: Uuid,
    pub source_id: Arc<str>,
    pub source_kind: Arc<str>,
    pub sender_id: Arc<str>,
}

impl From<&InboundEvent> for EventRef {
    fn from(e: &InboundEvent) -> Self {
        Self {
            id: e.id,
            source_id: Arc::from(e.source_id.as_str()),
            source_kind: Arc::from(e.source_kind.as_str()),
            sender_id: Arc::from(e.sender_id.as_str()),
        }
    }
}
