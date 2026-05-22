use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;
use tokio_stream::Stream;

use crate::error::{Result, RoyError};
use crate::event::TurnEvent;
use crate::provider::Provider;

/// How bytes move between us and the agent process. `PrintTransport` is the
/// headless `-p` stream-json driver; a `PtyTransport` slots in later for
/// claude's interactive subscription billing.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(
        &self,
        provider: Arc<dyn Provider>,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>>;
}

/// A live agent process. `send` writes one user turn and streams its events
/// until the provider reports turn end.
#[async_trait]
pub trait Handle: Send {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>>;
    async fn close(&mut self) -> Result<()>;
}

pub struct PrintTransport;

impl PrintTransport {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PrintTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for PrintTransport {
    async fn open(
        &self,
        provider: Arc<dyn Provider>,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>> {
        let cmd_name = provider.command().to_string();
        let args = provider.spawn_args(session_id, resume_cursor);

        let mut child = tokio::process::Command::new(&cmd_name)
            .args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| RoyError::Spawn { cmd: cmd_name, source })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        // Reader task: parse every stdout line into events, forward on a
        // channel for the whole process lifetime. `send` consumes per-turn.
        let (tx, rx) = mpsc::channel::<TurnEvent>(256);
        let reader_provider = Arc::clone(&provider);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ev) = reader_provider.parse_line(&line) {
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            }
        });

        Ok(Box::new(PrintHandle {
            child,
            stdin,
            rx,
            provider,
        }))
    }
}

pub struct PrintHandle {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<TurnEvent>,
    provider: Arc<dyn Provider>,
}

#[async_trait]
impl Handle for PrintHandle {
    async fn send(
        &mut self,
        _prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>> {
        unimplemented!("Task 6")
    }
    async fn close(&mut self) -> Result<()> {
        let _ = self.child.start_kill();
        Ok(())
    }
}
