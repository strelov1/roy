use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::Stream;
use uuid::Uuid;

use crate::error::Result;
use crate::event::TurnEvent;
use crate::transport::{Handle, Transport};

/// A conversation with one agent. Holds identity and the opaque
/// `resume_cursor` (claude puts its session id there). Lazily opens a live
/// process on the first `send`; subsequent sends reuse it for multi-turn.
pub struct Session {
    id: String,
    cwd: PathBuf,
    resume_cursor: Option<String>,
    transport: Arc<dyn Transport>,
    handle: Option<Box<dyn Handle>>,
}

impl Session {
    pub fn new(transport: Arc<dyn Transport>, cwd: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            cwd,
            resume_cursor: None,
            transport,
            handle: None,
        }
    }

    /// Re-open an EXISTING session by its id (e.g. after the host app
    /// restarted). The first `send` will spawn with `--resume <id>` because
    /// `resume_cursor` is pre-seeded.
    pub fn resume(transport: Arc<dyn Transport>, cwd: PathBuf, session_id: String) -> Self {
        Self {
            id: session_id.clone(),
            cwd,
            resume_cursor: Some(session_id),
            transport,
            handle: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn resume_cursor(&self) -> Option<&str> {
        self.resume_cursor.as_deref()
    }

    /// Send one user turn; returns a stream of events until turn end. Opens
    /// the process on first use (new session, or resume if a cursor is set).
    pub async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>> {
        if self.handle.is_none() {
            let handle = self
                .transport
                .open(&self.id, self.resume_cursor.as_deref(), self.cwd.clone())
                .await?;
            self.resume_cursor = handle.resume_cursor();
            self.handle = Some(handle);
        }
        self.handle.as_mut().unwrap().send(prompt).await
    }

    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut handle) = self.handle.take() {
            handle.close().await?;
        }
        Ok(())
    }
}
