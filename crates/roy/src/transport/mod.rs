use std::path::PathBuf;
use std::pin::Pin;

use async_trait::async_trait;
use tokio::sync::oneshot;
use tokio_stream::Stream;

use crate::error::Result;
use crate::event::TurnEvent;

pub mod acp;

pub use acp::{AcpConfig, AcpTransport, PermissionPolicy, SystemPromptChannel};

pub type TurnStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send + 'static>>;

/// Drop or `send(())` it to cancel the in-flight turn. The transport
/// translates this into an agent-side cancel (e.g. ACP `session/cancel`); the
/// stream stays open and yields the terminal `Result` after the agent winds
/// down, so consumers see a clean turn boundary either way.
pub type CancelSignal = oneshot::Sender<()>;

/// How bytes move between us and the agent process.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
        system_prompt: Option<&str>,
        extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<Box<dyn Handle>>;
}

/// A live agent process. `send` writes one user turn and returns the event
/// stream + the cancel handle. Callers that don't intend to cancel can ignore
/// the handle; dropping it has the same effect as an explicit cancel.
#[async_trait]
pub trait Handle: Send {
    async fn send(&mut self, prompt: &str) -> Result<(TurnStream, CancelSignal)>;
    /// Opaque token to resume THIS session on the next `open`. For ACP this is
    /// the agent-issued `sessionId` from `session/new`.
    fn resume_cursor(&self) -> Option<String>;
    async fn close(&mut self) -> Result<()>;
    /// Persona to inject as the first turn, set only when the transport could
    /// not apply it natively (FirstTurn channel) AND this was a fresh open.
    /// Drains on first call. `None` for Meta channels and all resumes.
    fn take_pending_persona(&mut self) -> Option<String>;
}
