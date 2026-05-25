//! Client to the roy daemon over its Unix socket. Wraps a long-held connection
//! that walks through Spawn/Resume → AcquireInput → Send → Frame loop →
//! ReleaseInput for a single turn.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use roy::control::{ClientCommand, ServerEvent};
use roy::event::TurnEvent;
use roy::journal::JournalEntry;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// A single-turn daemon connection. Owns one Unix-socket connection and walks
/// it through Spawn/Resume → AcquireInput → Send → Frame loop → ReleaseInput.
///
/// Replaces the v1 `Fire` composite, which couldn't be cancelled externally
/// because it opened and closed its own connection inside the daemon call.
#[async_trait]
pub trait Conn: Send {
    async fn spawn(&mut self, preset: &str, cwd: Option<PathBuf>) -> Result<String>;

    async fn resume(&mut self, session_id: &str) -> Result<String>;

    async fn acquire_input(&mut self, session: &str) -> Result<()>;

    async fn send_prompt(&mut self, session: &str, text: String) -> Result<()>;

    /// Returns `Ok(Some(event))` for each `Frame` from the daemon, including
    /// the terminal `Result`. After the terminal Result has been surfaced,
    /// subsequent calls return `Ok(None)`.
    async fn next_frame(&mut self) -> Result<Option<TurnEvent>>;

    async fn cancel_turn(&mut self, session: &str) -> Result<()>;

    async fn release_input(&mut self, session: &str) -> Result<()>;
}

#[async_trait]
pub trait ConnFactory: Send + Sync {
    type Conn: Conn;
    async fn open(&self) -> Result<Self::Conn>;
}

pub struct RealConnFactory {
    socket_path: PathBuf,
}

impl RealConnFactory {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

#[async_trait]
impl ConnFactory for RealConnFactory {
    type Conn = TurnConn<UnixStream>;
    async fn open(&self) -> Result<TurnConn<UnixStream>> {
        TurnConn::open(&self.socket_path).await
    }
}

pub struct TurnConn<S: AsyncRead + AsyncWrite + Unpin + Send> {
    write_half: tokio::io::WriteHalf<S>,
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<S>>>,
    terminal_seen: bool,
}

impl TurnConn<UnixStream> {
    pub async fn open(socket_path: &std::path::Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to daemon at {}", socket_path.display()))?;
        TurnConn::from_stream(stream)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> TurnConn<S> {
    pub fn from_stream(stream: S) -> Result<Self> {
        let (read_half, write_half) = tokio::io::split(stream);
        Ok(Self {
            write_half,
            lines: BufReader::new(read_half).lines(),
            terminal_seen: false,
        })
    }

    async fn send_cmd(&mut self, cmd: &ClientCommand) -> Result<()> {
        let line = serde_json::to_string(cmd).context("serializing ClientCommand")?;
        self.write_half.write_all(line.as_bytes()).await?;
        self.write_half.write_all(b"\n").await?;
        self.write_half.flush().await?;
        Ok(())
    }

    async fn read_event(&mut self) -> Result<ServerEvent> {
        let Some(raw) = self.lines.next_line().await? else {
            return Err(anyhow!("daemon closed connection"));
        };
        serde_json::from_str(&raw).with_context(|| format!("parsing daemon line: {raw}"))
    }
}

#[async_trait]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> Conn for TurnConn<S> {
    async fn spawn(&mut self, preset: &str, cwd: Option<PathBuf>) -> Result<String> {
        self.send_cmd(&ClientCommand::Spawn {
            agent: preset.into(),
            cwd,
            model: None,
            permission: None,
            resume: None,
            system_prompt: None,
        })
        .await?;
        loop {
            match self.read_event().await? {
                ServerEvent::Spawning { .. } => continue,
                ServerEvent::Spawned { session, .. } => return Ok(session),
                ServerEvent::Error { code, message, .. } => {
                    return Err(anyhow!("spawn failed: {code}: {message}"));
                }
                other => return Err(anyhow!("unexpected response to Spawn: {other:?}")),
            }
        }
    }

    async fn resume(&mut self, session_id: &str) -> Result<String> {
        self.send_cmd(&ClientCommand::Resume {
            session: session_id.into(),
        })
        .await?;
        loop {
            match self.read_event().await? {
                ServerEvent::Resuming { .. } => continue,
                ServerEvent::Resumed { session, .. } => return Ok(session),
                ServerEvent::Error { code, message, .. } => {
                    return Err(anyhow!("resume failed: {code}: {message}"));
                }
                other => return Err(anyhow!("unexpected response to Resume: {other:?}")),
            }
        }
    }

    async fn acquire_input(&mut self, session: &str) -> Result<()> {
        self.send_cmd(&ClientCommand::AcquireInput {
            session: session.into(),
        })
        .await?;
        match self.read_event().await? {
            ServerEvent::InputAcquired { acquired: true, .. } => Ok(()),
            ServerEvent::InputAcquired {
                acquired: false, ..
            } => Err(anyhow!("input lease busy")),
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("acquire_input failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to AcquireInput: {other:?}")),
        }
    }

    async fn send_prompt(&mut self, session: &str, text: String) -> Result<()> {
        self.send_cmd(&ClientCommand::Send {
            session: session.into(),
            text,
        })
        .await
    }

    async fn next_frame(&mut self) -> Result<Option<TurnEvent>> {
        if self.terminal_seen {
            return Ok(None);
        }
        match self.read_event().await? {
            ServerEvent::Frame {
                entry: JournalEntry { event, .. },
                ..
            } => {
                if matches!(event, TurnEvent::Result { .. }) {
                    self.terminal_seen = true;
                }
                Ok(Some(event))
            }
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("frame stream error: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected event during frame loop: {other:?}")),
        }
    }

    async fn cancel_turn(&mut self, session: &str) -> Result<()> {
        self.send_cmd(&ClientCommand::CancelTurn {
            session: session.into(),
        })
        .await
    }

    async fn release_input(&mut self, session: &str) -> Result<()> {
        self.send_cmd(&ClientCommand::ReleaseInput {
            session: session.into(),
        })
        .await?;
        match self.read_event().await? {
            ServerEvent::InputReleased { .. } => Ok(()),
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("release_input failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to ReleaseInput: {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy::event::{StopReason, TurnEvent};
    use tokio::io::AsyncWriteExt;

    use roy::journal::JournalEntry as JE;

    /// Scripted-daemon fixture: reads N lines, returns the i-th canned
    /// response for each. Caller's closure asserts on the parsed command.
    async fn scripted_daemon(
        server: tokio::io::DuplexStream,
        script: Vec<(Box<dyn FnOnce(ClientCommand) + Send>, ServerEvent)>,
    ) {
        let (r, mut w) = tokio::io::split(server);
        let mut lines = BufReader::new(r).lines();
        for (assert_cmd, response) in script {
            let raw = match lines.next_line().await {
                Ok(Some(line)) => line,
                _ => return,
            };
            let cmd: ClientCommand = serde_json::from_str(&raw).unwrap();
            assert_cmd(cmd);
            let line = serde_json::to_string(&response).unwrap();
            w.write_all(line.as_bytes()).await.unwrap();
            w.write_all(b"\n").await.unwrap();
            w.flush().await.unwrap();
        }
    }

    /// Build a TurnConn from a duplex client half.
    fn turn_conn_from_duplex(stream: tokio::io::DuplexStream) -> TurnConn<tokio::io::DuplexStream> {
        TurnConn::from_stream(stream).unwrap()
    }

    #[tokio::test]
    async fn turn_conn_spawn_returns_session_id() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(scripted_daemon(
            server,
            vec![(
                Box::new(|cmd| match cmd {
                    ClientCommand::Spawn { agent, cwd, .. } => {
                        assert_eq!(agent, "claude");
                        assert!(cwd.is_none());
                    }
                    other => panic!("expected Spawn, got {other:?}"),
                }),
                ServerEvent::Spawned {
                    session: "sid-1".into(),
                    resume_cursor: None,
                },
            )],
        ));
        let mut conn = turn_conn_from_duplex(client);
        let sid = conn.spawn("claude", None).await.unwrap();
        assert_eq!(sid, "sid-1");
    }

    #[tokio::test]
    async fn turn_conn_next_frame_surfaces_terminal_then_none() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(scripted_daemon(
            server,
            vec![
                (
                    Box::new(|_| {}),
                    ServerEvent::InputAcquired {
                        session: "sid".into(),
                        acquired: true,
                    },
                ),
                (
                    Box::new(|_| {}),
                    ServerEvent::Frame {
                        session: "sid".into(),
                        entry: JE {
                            seq: 1,
                            event: TurnEvent::Result {
                                cost_usd: None,
                                stop_reason: StopReason::EndTurn,
                            },
                        },
                    },
                ),
            ],
        ));
        let mut conn = turn_conn_from_duplex(client);
        conn.acquire_input("sid").await.unwrap();
        // Send a prompt to drive the next scripted response
        let _ = conn.send_prompt("sid", "ping".into()).await;
        let frame = conn.next_frame().await.unwrap();
        assert!(matches!(frame, Some(TurnEvent::Result { .. })));
        let next = conn.next_frame().await.unwrap();
        assert!(next.is_none(), "expected None after terminal Result");
    }

    #[tokio::test]
    async fn turn_conn_acquire_input_fails_on_busy() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(scripted_daemon(
            server,
            vec![(
                Box::new(|_| {}),
                ServerEvent::InputAcquired {
                    session: "sid".into(),
                    acquired: false,
                },
            )],
        ));
        let mut conn = turn_conn_from_duplex(client);
        let err = conn.acquire_input("sid").await.unwrap_err();
        assert!(err.to_string().contains("busy"));
    }
}
