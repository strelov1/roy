use std::path::PathBuf;

use async_trait::async_trait;
use tokio_stream::Stream;

use crate::error::Result;
use crate::event::TurnEvent;

pub mod acp;
pub mod print;

pub use acp::{AcpConfig, AcpTransport};
pub use print::PrintTransport;

/// How bytes move between us and the agent process.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>>;
}

/// A live agent process. `send` writes one user turn and streams its events
/// until turn end.
#[async_trait]
pub trait Handle: Send {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>>;
    /// Opaque token to resume THIS session on the next `open`. claude: the
    /// session id; gemini: the ACP sessionId from session/new.
    fn resume_cursor(&self) -> Option<String>;
    async fn close(&mut self) -> Result<()>;
}
