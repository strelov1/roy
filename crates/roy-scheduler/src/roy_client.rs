//! Roy daemon client used by the driver. The only roy import surface
//! allowed in this crate (besides protocol types) is the UDS shape —
//! `ClientCommand` in, `ServerEvent` out, JSON over newline-delimited
//! frames.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use roy::{ClientCommand, FireTarget, ServerEvent, TurnEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::UnixStream;

/// Connect to the daemon, write one command frame, and hand back the reply line
/// reader. Shared by `fire` and `inject` — both speak the same connect → send →
/// read-until-terminal-event shape.
async fn connect_and_send(
    socket_path: &Path,
    cmd: &ClientCommand,
) -> Result<Lines<BufReader<OwnedReadHalf>>> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to roy daemon at {}", socket_path.display()))?;
    let (reader, mut writer) = stream.into_split();
    let line = serde_json::to_string(cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(BufReader::new(reader).lines())
}

/// Successful Fire result — the turn finished with a terminal Result.
#[derive(Debug, Clone)]
pub struct FireSuccess {
    pub session_id: String,
    pub seq_range: (u64, u64),
    pub cost_usd: Option<f64>,
    pub stop_reason: String,
    pub assistant_text: String,
}

/// Outcome of a Fire call, mapped from the three ServerEvent variants.
#[derive(Debug, Clone)]
pub enum FireOutcome {
    Done(FireSuccess),
    Timeout {
        session_id: String,
        partial_seq_range: (u64, u64),
    },
    Error {
        session_id: Option<String>,
        code: String,
        message: String,
    },
}

/// Outcome of an Inject call. `Noted` is the respond=false reply; the other
/// three mirror Fire for respond=true.
#[derive(Debug, Clone)]
pub enum InjectOutcome {
    Noted {
        session_id: String,
        seq: u64,
    },
    Done(FireSuccess),
    Timeout {
        session_id: String,
        partial_seq_range: (u64, u64),
    },
    Error {
        session_id: Option<String>,
        code: String,
        message: String,
    },
}

pub async fn fire(
    socket_path: &Path,
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
    timeout: Duration,
) -> Result<FireOutcome> {
    let cmd = ClientCommand::Fire {
        target,
        prompt,
        tags,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let mut lines = connect_and_send(socket_path, &cmd).await?;

    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up before terminal Fire event"))?;
        let evt: ServerEvent = serde_json::from_str(raw.trim())?;
        match evt {
            ServerEvent::FireDone {
                session,
                seq_range,
                result,
                assistant_text,
            } => {
                let TurnEvent::Result {
                    cost_usd,
                    stop_reason,
                } = result
                else {
                    return Err(anyhow!("non-Result in FireDone"));
                };
                return Ok(FireOutcome::Done(FireSuccess {
                    session_id: session,
                    seq_range,
                    cost_usd,
                    stop_reason: format!("{stop_reason:?}"),
                    assistant_text,
                }));
            }
            ServerEvent::FireTimeout {
                session,
                partial_seq_range,
            } => {
                return Ok(FireOutcome::Timeout {
                    session_id: session,
                    partial_seq_range,
                });
            }
            ServerEvent::FireError {
                session,
                code,
                message,
            } => {
                return Ok(FireOutcome::Error {
                    session_id: session,
                    code: code.to_string(),
                    message,
                });
            }
            // Daemon may emit unrelated frames if we share a connection,
            // but for a fresh Fire-only connection there shouldn't be any.
            _ => continue,
        }
    }
}

pub async fn inject(
    socket_path: &Path,
    session: String,
    text: String,
    source_session: Option<String>,
    respond: bool,
    timeout: Duration,
) -> Result<InjectOutcome> {
    let cmd = ClientCommand::Inject {
        session,
        text,
        source_session,
        respond,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let mut lines = connect_and_send(socket_path, &cmd).await?;

    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up before terminal Inject event"))?;
        let evt: ServerEvent = serde_json::from_str(raw.trim())?;
        match evt {
            ServerEvent::Injected { session, seq } => {
                return Ok(InjectOutcome::Noted {
                    session_id: session,
                    seq,
                });
            }
            ServerEvent::FireDone {
                session,
                seq_range,
                result,
                assistant_text,
            } => {
                let TurnEvent::Result {
                    cost_usd,
                    stop_reason,
                } = result
                else {
                    return Err(anyhow!("non-Result in FireDone"));
                };
                return Ok(InjectOutcome::Done(FireSuccess {
                    session_id: session,
                    seq_range,
                    cost_usd,
                    stop_reason: format!("{stop_reason:?}"),
                    assistant_text,
                }));
            }
            ServerEvent::FireTimeout {
                session,
                partial_seq_range,
            } => {
                return Ok(InjectOutcome::Timeout {
                    session_id: session,
                    partial_seq_range,
                });
            }
            ServerEvent::FireError {
                session,
                code,
                message,
            }
            | ServerEvent::Error {
                session,
                code,
                message,
            } => {
                return Ok(InjectOutcome::Error {
                    session_id: session,
                    code: code.to_string(),
                    message,
                });
            }
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::net::UnixListener;

    /// Spawn a mock daemon listening on `path` that reads one ClientCommand
    /// and writes one ServerEvent in JSON-line frames.
    async fn spawn_mock(path: std::path::PathBuf, reply: ServerEvent) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _cmd_line = lines.next_line().await.unwrap();
            let out = serde_json::to_string(&reply).unwrap();
            wr.write_all(out.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    #[tokio::test]
    async fn fire_done_maps_to_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::FireDone {
                session: "sid".into(),
                seq_range: (1, 5),
                result: TurnEvent::Result {
                    cost_usd: Some(0.01),
                    stop_reason: roy::StopReason::EndTurn,
                },
                assistant_text: "hi".into(),
            },
        )
        .await;

        let out = fire(
            &path,
            FireTarget::Spawn {
                preset: "claude".into(),
                project_id: None,
                system_prompt: None,
            },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        match out {
            FireOutcome::Done(s) => {
                assert_eq!(s.session_id, "sid");
                assert_eq!(s.assistant_text, "hi");
                assert_eq!(s.seq_range, (1, 5));
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fire_timeout_maps_to_timeout() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::FireTimeout {
                session: "sid".into(),
                partial_seq_range: (1, 3),
            },
        )
        .await;

        let out = fire(
            &path,
            FireTarget::Spawn {
                preset: "claude".into(),
                project_id: None,
                system_prompt: None,
            },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(matches!(out, FireOutcome::Timeout { .. }));
    }

    #[tokio::test]
    async fn fire_error_maps_to_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::FireError {
                session: None,
                code: roy::ErrorCode::SpawnFailed,
                message: "boom".into(),
            },
        )
        .await;

        let out = fire(
            &path,
            FireTarget::Spawn {
                preset: "claude".into(),
                project_id: None,
                system_prompt: None,
            },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(matches!(out, FireOutcome::Error { .. }));
    }

    #[tokio::test]
    async fn inject_note_maps_to_noted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::Injected {
                session: "sid".into(),
                seq: 7,
            },
        )
        .await;

        let out = inject(
            &path,
            "sid".into(),
            "bg result".into(),
            Some("child".into()),
            false,
            Duration::from_secs(60),
        )
        .await
        .unwrap();

        match out {
            InjectOutcome::Noted { session_id, seq } => {
                assert_eq!(session_id, "sid");
                assert_eq!(seq, 7);
            }
            other => panic!("expected Noted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_daemon_at_path_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.sock");
        let r = fire(
            &path,
            FireTarget::Spawn {
                preset: "claude".into(),
                project_id: None,
                system_prompt: None,
            },
            "p".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await;
        assert!(r.is_err());
    }
}
