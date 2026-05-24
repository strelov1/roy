//! Subscriber dispatcher. Called by `driver::invoke_fire` after a Fire
//! completes. Loads enabled subscribers (agent OR trigger scope), iterates
//! in `order_index ASC, created_at ASC`, executes per-kind, writes a
//! `fire_subscriber_runs` row per attempt. At-most-once per fire — no
//! retry in v1.

use async_trait::async_trait;

pub mod dispatch;
pub mod inject_parent;
pub mod notify_native;
pub mod registry;
pub mod webhook;

pub use dispatch::dispatch;

#[async_trait]
pub trait Subscriber: Send + Sync {
    async fn run(&self, ctx: &FireCtx<'_>) -> Outcome;
}

pub struct FireCtx<'a> {
    pub socket_path: &'a std::path::Path,
    pub fire: &'a crate::types::Fire,
    pub agent_name: &'a str,
    pub success: Option<&'a crate::roy_client::FireSuccess>,
    pub error_message: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Ok,
    Error,
    Skipped,
}

impl RunStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Error => "error",
            RunStatus::Skipped => "skipped",
        }
    }
}

pub struct Outcome {
    pub status: RunStatus,
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

impl Outcome {
    pub fn ok() -> Self {
        Self {
            status: RunStatus::Ok,
            error_message: None,
            response_snippet: None,
        }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            status: RunStatus::Error,
            error_message: Some(msg.into()),
            response_snippet: None,
        }
    }
    pub fn skipped(msg: impl Into<String>) -> Self {
        Self {
            status: RunStatus::Skipped,
            error_message: Some(msg.into()),
            response_snippet: None,
        }
    }
}
