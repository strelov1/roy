//! Daemon client. Opens one short-lived UDS connection per fire, sends
//! ClientCommand::Fire, drains ServerEvents, calls into the ReplyHook for
//! each Frame and on the terminal event.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use roy_protocol::{ClientCommand, FireTarget, ServerEvent, TurnEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::reply::{FireOutcome, ReplyHook};

#[derive(Debug, Clone)]
pub struct FireResult {
    pub outcome_kind: OutcomeKind,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeKind {
    Ok,
    Timeout,
    DaemonError(String),
    Cancelled,
}

pub async fn fire_with_hook(
    socket_path: &Path,
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
    timeout: Duration,
    mut hook: Box<dyn ReplyHook>,
    reply: crate::bus::ReplyHandle,
) -> Result<FireResult> {
    let cmd = ClientCommand::Fire {
        target,
        prompt,
        tags,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to daemon at {}", socket_path.display()))?;
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    wr.write_all(&roy_protocol::wire::encode_line(&cmd)?)
        .await?;
    wr.flush().await?;

    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up before terminal Fire event"))?;
        let evt: ServerEvent = roy_protocol::wire::decode_line(&raw)?;
        match evt {
            ServerEvent::Frame { entry, .. } => {
                hook.on_turn_event(&entry.event).await?;
            }
            ServerEvent::FireDone {
                session,
                result,
                assistant_text,
                ..
            } => {
                let TurnEvent::Result {
                    cost_usd,
                    stop_reason,
                } = result
                else {
                    return Err(anyhow!("non-Result in FireDone"));
                };
                hook.on_finish(
                    FireOutcome::Ok {
                        assistant_text,
                        cost_usd,
                        stop_reason: stop_reason.as_wire().to_string(),
                    },
                    reply,
                )
                .await?;
                return Ok(FireResult {
                    outcome_kind: OutcomeKind::Ok,
                    session_id: Some(session),
                });
            }
            ServerEvent::FireTimeout { session, .. } => {
                hook.on_finish(FireOutcome::Timeout { partial_text: None }, reply)
                    .await?;
                return Ok(FireResult {
                    outcome_kind: OutcomeKind::Timeout,
                    session_id: Some(session),
                });
            }
            ServerEvent::FireError {
                session,
                code,
                message,
            } => {
                hook.on_finish(
                    FireOutcome::DaemonError {
                        code: code.clone(),
                        message: message.clone(),
                    },
                    reply,
                )
                .await?;
                return Ok(FireResult {
                    outcome_kind: OutcomeKind::DaemonError(code.to_string()),
                    session_id: session,
                });
            }
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use roy_protocol::{ErrorCode, StopReason};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixListener;
    use tokio::sync::oneshot;

    struct CapturingHook {
        captured: Arc<Mutex<Option<FireOutcome>>>,
    }

    #[async_trait]
    impl ReplyHook for CapturingHook {
        async fn on_turn_event(&mut self, _ev: &TurnEvent) -> Result<()> {
            Ok(())
        }
        async fn on_finish(
            self: Box<Self>,
            outcome: FireOutcome,
            _reply: crate::bus::ReplyHandle,
        ) -> Result<()> {
            *self.captured.lock().unwrap() = Some(outcome);
            Ok(())
        }
    }

    async fn mock_daemon(path: PathBuf, reply: ServerEvent) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _ = lines.next_line().await.unwrap();
            let line = serde_json::to_string(&reply).unwrap();
            wr.write_all(line.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    #[tokio::test]
    async fn fire_done_hits_hook_and_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.sock");
        mock_daemon(
            p.clone(),
            ServerEvent::FireDone {
                session: "sid".into(),
                seq_range: (1, 3),
                result: TurnEvent::Result {
                    cost_usd: Some(0.01),
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "hi".into(),
            },
        )
        .await;
        let captured: Arc<Mutex<Option<FireOutcome>>> = Arc::new(Mutex::new(None));
        let hook = Box::new(CapturingHook {
            captured: captured.clone(),
        });
        let (tx, _rx) = oneshot::channel::<crate::bus::HttpReply>();
        let result = fire_with_hook(
            &p,
            FireTarget::Spawn {
                harness: "claude".into(),
                system_prompt: None,
            },
            "hello".into(),
            Default::default(),
            std::time::Duration::from_secs(5),
            hook,
            crate::bus::ReplyHandle::HttpSync(tx),
        )
        .await
        .unwrap();
        assert_eq!(result.outcome_kind, OutcomeKind::Ok);
        assert_eq!(result.session_id.as_deref(), Some("sid"));
        let outcome = captured.lock().unwrap().clone().unwrap();
        match outcome {
            FireOutcome::Ok {
                assistant_text,
                stop_reason,
                ..
            } => {
                assert_eq!(assistant_text, "hi");
                // Regression guard: stop_reason must use the snake_case wire
                // vocabulary (`StopReason::as_wire`), not the Rust Debug form.
                assert_eq!(stop_reason, "end_turn");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fire_error_returns_daemon_error_kind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.sock");
        mock_daemon(
            p.clone(),
            ServerEvent::FireError {
                session: None,
                code: ErrorCode::NoSession,
                message: "gone".into(),
            },
        )
        .await;
        let captured = Arc::new(Mutex::new(None));
        let hook = Box::new(CapturingHook {
            captured: captured.clone(),
        });
        let (tx, _rx) = oneshot::channel::<crate::bus::HttpReply>();
        let result = fire_with_hook(
            &p,
            FireTarget::Spawn {
                harness: "claude".into(),
                system_prompt: None,
            },
            "x".into(),
            Default::default(),
            std::time::Duration::from_secs(5),
            hook,
            crate::bus::ReplyHandle::HttpSync(tx),
        )
        .await
        .unwrap();
        assert_eq!(
            result.outcome_kind,
            OutcomeKind::DaemonError("no_session".into())
        );
    }
}
