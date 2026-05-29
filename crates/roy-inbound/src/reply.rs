//! ReplyHook contract. One hook instance lives per fire; the dispatcher
//! calls `on_turn_event` for every intermediate `TurnEvent` (currently
//! unused — see spec on streaming) and `on_finish` exactly once.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use roy_protocol::event::TurnEvent;
use roy_protocol::ErrorCode;

use crate::bus::{EventRef, ReplyHandle};

#[derive(Debug, Clone)]
pub enum FireOutcome {
    Ok {
        assistant_text: String,
        cost_usd: Option<f64>,
        stop_reason: String,
    },
    DaemonError {
        code: ErrorCode,
        message: String,
    },
    Timeout {
        partial_text: Option<String>,
    },
    Cancelled,
    RouteRejected,
}

#[async_trait]
pub trait ReplyHook: Send {
    async fn on_turn_event(&mut self, ev: &TurnEvent) -> Result<()>;
    async fn on_finish(self: Box<Self>, outcome: FireOutcome, reply: ReplyHandle) -> Result<()>;
}

/// Per-source-kind factory. The dispatcher consults the registry to build
/// a fresh hook for every event.
pub type ReplyHookFactory = Box<dyn Fn(&EventRef) -> Box<dyn ReplyHook> + Send + Sync>;

pub struct ReplyHookRegistry {
    factories: HashMap<String, ReplyHookFactory>,
}

impl ReplyHookRegistry {
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    pub fn register(&mut self, kind: &str, factory: ReplyHookFactory) {
        self.factories.insert(kind.into(), factory);
    }

    pub fn make(&self, kind: &str, ev: &EventRef) -> Option<Box<dyn ReplyHook>> {
        self.factories.get(kind).map(|f| f(ev))
    }
}

impl Default for ReplyHookRegistry {
    fn default() -> Self {
        Self::new()
    }
}
