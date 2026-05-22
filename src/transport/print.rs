use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;
use tokio_stream::Stream;

use super::{Handle, Transport};
use crate::error::{Result, RoyError};
use crate::event::TurnEvent;
use crate::provider::Provider;

pub struct PrintTransport {
    provider: Arc<dyn Provider>,
}

impl PrintTransport {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Transport for PrintTransport {
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>> {
        let provider = Arc::clone(&self.provider);
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
            session_id: session_id.to_string(),
        }))
    }
}

pub struct PrintHandle {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<TurnEvent>,
    provider: Arc<dyn Provider>,
    session_id: String,
}

#[async_trait]
impl Handle for PrintHandle {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>> {
        let line = self.provider.encode_user_message(prompt);
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let provider = Arc::clone(&self.provider);
        let rx = &mut self.rx;
        let stream = async_stream::stream! {
            while let Some(ev) = rx.recv().await {
                let end = provider.is_turn_end(&ev);
                yield ev;
                if end {
                    break;
                }
            }
        };
        Ok(Box::pin(stream))
    }

    fn resume_cursor(&self) -> Option<String> {
        Some(self.session_id.clone())
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.child.start_kill();
        Ok(())
    }
}
