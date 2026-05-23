use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_stream::Stream;

use crate::error::Result;
use crate::event::TurnEvent;

pub mod acp;
pub mod print;

pub use acp::{AcpConfig, AcpTransport, PermissionPolicy};
pub use print::PrintTransport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrMode {
    Null,
    Inherit,
}

impl StderrMode {
    pub(crate) fn stdio(self) -> Stdio {
        match self {
            StderrMode::Null => Stdio::null(),
            StderrMode::Inherit => Stdio::inherit(),
        }
    }
}

pub type TurnStream<'a> = Pin<Box<dyn Stream<Item = TurnEvent> + Send + 'a>>;

pub(crate) fn borrowed_event_stream<'a, F>(
    rx: &'a mut mpsc::Receiver<TurnEvent>,
    mut is_turn_end: F,
) -> TurnStream<'a>
where
    F: FnMut(&TurnEvent) -> bool + Send + 'a,
{
    Box::pin(async_stream::stream! {
        while let Some(ev) = rx.recv().await {
            let end = is_turn_end(&ev);
            yield ev;
            if end {
                break;
            }
        }
    })
}

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
    async fn send(&mut self, prompt: &str) -> Result<TurnStream<'_>>;
    /// Opaque token to resume THIS session on the next `open`. claude: the
    /// session id; gemini: the ACP sessionId from session/new.
    fn resume_cursor(&self) -> Option<String>;
    async fn close(&mut self) -> Result<()>;
}
