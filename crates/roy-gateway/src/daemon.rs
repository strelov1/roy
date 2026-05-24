//! Client to the roy daemon over its Unix socket. Wraps a single
//! `ClientCommand::Fire` (composite Spawn-or-Resume + WaitForResult)
//! so the gateway can stay synchronous-per-message at the daemon API.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use roy::control::{ClientCommand, ErrorCode, FireTarget, ServerEvent};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

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
}
