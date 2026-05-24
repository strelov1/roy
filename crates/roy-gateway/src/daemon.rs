//! Client to the roy daemon over its Unix socket. Wraps a single
//! `ClientCommand::Fire` (composite Spawn-or-Resume + WaitForResult)
//! so the gateway can stay synchronous-per-message at the daemon API.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use roy::control::{ClientCommand, ErrorCode, FireTarget, ServerEvent};
use roy::event::TurnEvent;
use roy::journal::JournalEntry;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Clone)]
pub enum FireOutcome {
    Done {
        session: String,
        assistant_text: String,
    },
    Timeout {
        session: Option<String>,
    },
    Error {
        session: Option<String>,
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

/// Transport-agnostic Fire. Public so unit tests can drive it through
/// `tokio::io::duplex` without a real Unix socket.
pub async fn fire_via_stream<S>(
    stream: S,
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
    timeout: Duration,
) -> Result<FireOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();

    let cmd = ClientCommand::Fire {
        target,
        prompt,
        tags,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let line = serde_json::to_string(&cmd).context("serializing Fire")?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    let Some(raw) = lines.next_line().await? else {
        return Err(anyhow!("daemon closed connection before Fire response"));
    };
    let evt: ServerEvent =
        serde_json::from_str(&raw).with_context(|| format!("parsing daemon line: {raw}"))?;
    match evt {
        ServerEvent::FireDone {
            session,
            assistant_text,
            ..
        } => Ok(FireOutcome::Done {
            session,
            assistant_text,
        }),
        ServerEvent::FireTimeout { session, .. } => Ok(FireOutcome::Timeout {
            session: Some(session),
        }),
        ServerEvent::FireError {
            session,
            code,
            message,
        } => Ok(FireOutcome::Error {
            session,
            code,
            message,
        }),
        ServerEvent::Error {
            session,
            code,
            message,
        } => Ok(FireOutcome::Error {
            session,
            code,
            message,
        }),
        other => Err(anyhow!("unexpected daemon event for Fire: {other:?}")),
    }
}

impl DaemonClient {
    pub async fn fire_resume(
        &self,
        session_id: &str,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connecting to daemon at {}", self.socket_path.display()))?;
        fire_via_stream(
            stream,
            FireTarget::Resume {
                session_id: session_id.into(),
            },
            prompt,
            tags,
            timeout,
        )
        .await
    }

    pub async fn fire_spawn(
        &self,
        preset: &str,
        project_id: Option<String>,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connecting to daemon at {}", self.socket_path.display()))?;
        fire_via_stream(
            stream,
            FireTarget::Spawn {
                preset: preset.into(),
                project_id,
            },
            prompt,
            tags,
            timeout,
        )
        .await
    }
}

/// A single-turn daemon connection. Owns one Unix-socket connection and walks
/// it through Spawn/Resume → AcquireInput → Send → Frame loop → ReleaseInput.
///
/// Replaces the v1 `Fire` composite, which couldn't be cancelled externally
/// because it opened and closed its own connection inside the daemon call.
#[async_trait]
pub trait Conn: Send {
    async fn spawn(
        &mut self,
        preset: &str,
        project_id: Option<String>,
        tags: BTreeMap<String, String>,
    ) -> Result<String>;

    async fn resume(&mut self, session_id: &str, tags: BTreeMap<String, String>) -> Result<String>;

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
    async fn spawn(
        &mut self,
        preset: &str,
        project_id: Option<String>,
        tags: BTreeMap<String, String>,
    ) -> Result<String> {
        self.send_cmd(&ClientCommand::Spawn {
            agent: preset.into(),
            project_id,
            model: None,
            permission: None,
            resume: None,
            tags,
        })
        .await?;
        match self.read_event().await? {
            ServerEvent::Spawned { session, .. } => Ok(session),
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("spawn failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to Spawn: {other:?}")),
        }
    }

    async fn resume(&mut self, session_id: &str, tags: BTreeMap<String, String>) -> Result<String> {
        self.send_cmd(&ClientCommand::Resume {
            session: session_id.into(),
            tags: Some(tags),
        })
        .await?;
        match self.read_event().await? {
            ServerEvent::Resumed { session, .. } => Ok(session),
            ServerEvent::Error { code, message, .. } => {
                Err(anyhow!("resume failed: {code}: {message}"))
            }
            other => Err(anyhow!("unexpected response to Resume: {other:?}")),
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

    /// Spawn a fake daemon on one half of a duplex pair. Reads one JSON
    /// line, asserts via the caller-supplied closure, then writes the
    /// caller-supplied response.
    async fn fake_daemon<F>(server: tokio::io::DuplexStream, response: ServerEvent, assert_cmd: F)
    where
        F: FnOnce(ClientCommand) + Send + 'static,
    {
        let (r, mut w) = tokio::io::split(server);
        let mut lines = BufReader::new(r).lines();
        let raw = lines.next_line().await.unwrap().unwrap();
        let cmd: ClientCommand = serde_json::from_str(&raw).unwrap();
        assert_cmd(cmd);
        let line = serde_json::to_string(&response).unwrap();
        w.write_all(line.as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        w.flush().await.unwrap();
    }

    #[tokio::test]
    async fn fire_spawn_returns_done() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireDone {
                session: "abc".into(),
                seq_range: (0, 5),
                result: TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "hello world".into(),
            },
            |cmd| match cmd {
                ClientCommand::Fire {
                    target: FireTarget::Spawn { preset, project_id },
                    prompt,
                    timeout_ms,
                    ..
                } => {
                    assert_eq!(preset, "claude");
                    assert_eq!(project_id.as_deref(), None);
                    assert_eq!(prompt, "ping");
                    assert_eq!(timeout_ms, Some(30_000));
                }
                other => panic!("expected Fire::Spawn, got {other:?}"),
            },
        ));

        let out = fire_via_stream(
            client,
            FireTarget::Spawn {
                preset: "claude".into(),
                project_id: None,
            },
            "ping".into(),
            BTreeMap::new(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();

        match out {
            FireOutcome::Done {
                session,
                assistant_text,
            } => {
                assert_eq!(session, "abc");
                assert_eq!(assistant_text, "hello world");
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fire_resume_returns_done() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireDone {
                session: "abc".into(),
                seq_range: (10, 15),
                result: TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "resumed reply".into(),
            },
            |cmd| match cmd {
                ClientCommand::Fire {
                    target: FireTarget::Resume { session_id },
                    prompt,
                    ..
                } => {
                    assert_eq!(session_id, "abc");
                    assert_eq!(prompt, "follow-up");
                }
                other => panic!("expected Fire::Resume, got {other:?}"),
            },
        ));

        let out = fire_via_stream(
            client,
            FireTarget::Resume {
                session_id: "abc".into(),
            },
            "follow-up".into(),
            BTreeMap::new(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();

        assert!(matches!(out, FireOutcome::Done { session, assistant_text }
            if session == "abc" && assistant_text == "resumed reply"));
    }

    #[tokio::test]
    async fn fire_error_is_returned_verbatim() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireError {
                session: Some("sid".into()),
                code: ErrorCode::SpawnFailed,
                message: "agent crashed".into(),
            },
            |_| {},
        ));
        let out = fire_via_stream(
            client,
            FireTarget::Spawn {
                preset: "claude".into(),
                project_id: None,
            },
            "x".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        match out {
            FireOutcome::Error {
                session,
                code,
                message,
            } => {
                assert_eq!(session.as_deref(), Some("sid"));
                assert_eq!(code, ErrorCode::SpawnFailed);
                assert_eq!(message, "agent crashed");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fire_timeout_is_mapped() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireTimeout {
                session: "sid".into(),
                partial_seq_range: (0, 0),
            },
            |_| {},
        ));
        let out = fire_via_stream(
            client,
            FireTarget::Resume {
                session_id: "sid".into(),
            },
            "x".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        match out {
            FireOutcome::Timeout { session } => {
                assert_eq!(session.as_deref(), Some("sid"));
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generic_error_event_is_mapped() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::Error {
                session: None,
                code: ErrorCode::BadRequest,
                message: "nope".into(),
            },
            |_| {},
        ));
        let out = fire_via_stream(
            client,
            FireTarget::Spawn {
                preset: "claude".into(),
                project_id: None,
            },
            "x".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        match out {
            FireOutcome::Error {
                session,
                code,
                message,
            } => {
                assert!(session.is_none());
                assert_eq!(code, ErrorCode::BadRequest);
                assert_eq!(message, "nope");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

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
                    ClientCommand::Spawn {
                        agent, project_id, ..
                    } => {
                        assert_eq!(agent, "claude");
                        assert_eq!(project_id.as_deref(), None);
                    }
                    other => panic!("expected Spawn, got {other:?}"),
                }),
                ServerEvent::Spawned {
                    session: "sid-1".into(),
                    project_id: None,
                    resume_cursor: None,
                },
            )],
        ));
        let mut conn = turn_conn_from_duplex(client);
        let sid = conn.spawn("claude", None, BTreeMap::new()).await.unwrap();
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
