//! `roy serve` daemon: owns one `SessionManager` and serves connections from
//! triggers (Unix socket today, WebSocket next) speaking the control protocol
//! defined in `crate::control`.
//!
//! Wire format on Unix socket: one JSON object per line (`\n`-delimited).
//! Same payload is used over WebSocket frames — only the framing differs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

use crate::control::{ClientCommand, ServerEvent};
use crate::engine::InputLease;
use crate::error::{Result, RoyError};
use crate::manager::SessionManager;
use crate::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};

/// How the daemon builds a `Transport` from an agent name. Pluggable so the
/// daemon can be tested against fake agents without touching global state.
pub trait TransportFactory: Send + Sync {
    fn build(
        &self,
        agent: &str,
        model: Option<&str>,
        permission: Option<&str>,
    ) -> Result<Arc<dyn Transport>>;
}

/// Default mapping `agent name → AcpConfig` for the four ACP presets.
pub struct DefaultTransportFactory;

impl TransportFactory for DefaultTransportFactory {
    fn build(
        &self,
        agent: &str,
        _model: Option<&str>,
        permission: Option<&str>,
    ) -> Result<Arc<dyn Transport>> {
        let mut config = match agent {
            "claude_agent" => AcpConfig::claude_agent(),
            "gemini" => AcpConfig::gemini(),
            "opencode" => AcpConfig::opencode(),
            "codex" => AcpConfig::codex(),
            other => {
                return Err(RoyError::Protocol(format!("unknown agent: {other}")));
            }
        };
        if let Some(p) = permission {
            config.permission_policy = match p {
                "allow" => PermissionPolicy::AllowAll,
                "deny" => PermissionPolicy::Deny,
                other => {
                    return Err(RoyError::Protocol(format!(
                        "permission must be 'allow' or 'deny', got '{other}'"
                    )));
                }
            };
        }
        Ok(Arc::new(AcpTransport::new(config)))
    }
}

/// The daemon. Holds the shared manager and the transport factory; you can
/// either drive it over a Unix listener (`run_unix`) or pump a single
/// connection by hand (`serve_connection`, useful in tests).
pub struct Daemon {
    pub manager: Arc<SessionManager>,
    pub factory: Arc<dyn TransportFactory>,
}

impl Daemon {
    pub fn new(journal_dir: PathBuf, factory: Arc<dyn TransportFactory>) -> Self {
        Self {
            manager: Arc::new(SessionManager::new(journal_dir)),
            factory,
        }
    }

    /// Listen on a Unix socket, accept connections forever. Single-instance
    /// guard: if `socket_path` already exists and someone is listening, the
    /// bind fails — callers should treat that as `already running`.
    pub async fn run_unix(self: Arc<Self>, socket_path: &Path) -> Result<()> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(RoyError::Io)?;
        }
        // Best-effort cleanup of a stale socket file from a previous run.
        // The bind itself is the actual single-instance gate.
        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path).map_err(RoyError::Io)?;

        loop {
            let (stream, _) = listener.accept().await.map_err(RoyError::Io)?;
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                let (reader, writer) = stream.into_split();
                let _ = me.serve_connection(reader, writer).await;
            });
        }
    }

    /// Drive one client connection to completion. Used by `run_unix` for each
    /// accept; also called directly from tests over `tokio::io::duplex`.
    pub async fn serve_connection<R, W>(self: &Arc<Self>, reader: R, writer: W) -> Result<()>
    where
        R: AsyncRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let writer = Arc::new(Mutex::new(writer));
        let mut lines = BufReader::new(reader).lines();
        let mut subs: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
        let mut leases: HashMap<String, InputLease> = HashMap::new();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let cmd: ClientCommand = match serde_json::from_str(line) {
                Ok(c) => c,
                Err(e) => {
                    let _ = send(
                        &writer,
                        &ServerEvent::Error {
                            session: None,
                            code: "bad_request".into(),
                            message: e.to_string(),
                        },
                    )
                    .await;
                    continue;
                }
            };
            self.handle(cmd, &writer, &mut subs, &mut leases).await;
        }

        for handle in subs.into_values() {
            handle.abort();
        }
        // Leases drop automatically here, releasing engine writers.
        Ok(())
    }

    async fn handle<W>(
        self: &Arc<Self>,
        cmd: ClientCommand,
        writer: &Arc<Mutex<W>>,
        subs: &mut HashMap<String, tokio::task::JoinHandle<()>>,
        leases: &mut HashMap<String, InputLease>,
    ) where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        match cmd {
            ClientCommand::Spawn {
                agent,
                cwd,
                model,
                permission,
                resume: _,
            } => {
                // resume not yet wired — we'd need a SessionManager API that
                // accepts a resume cursor. Out of scope this iteration.
                let transport =
                    match self
                        .factory
                        .build(&agent, model.as_deref(), permission.as_deref())
                    {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = send(
                                writer,
                                &ServerEvent::Error {
                                    session: None,
                                    code: "spawn_failed".into(),
                                    message: e.to_string(),
                                },
                            )
                            .await;
                            return;
                        }
                    };
                let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });
                let engine = match self.manager.spawn(transport, cwd, 256, 1024).await {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = send(
                            writer,
                            &ServerEvent::Error {
                                session: None,
                                code: "spawn_failed".into(),
                                message: e.to_string(),
                            },
                        )
                        .await;
                        return;
                    }
                };
                let session = engine.id().to_string();
                let resume_cursor = engine.resume_cursor().await;
                let _ = send(
                    writer,
                    &ServerEvent::Spawned {
                        session,
                        resume_cursor,
                    },
                )
                .await;
            }

            ClientCommand::Attach { session, from_seq } => {
                let engine = match self.manager.get(&session).await {
                    Some(e) => e,
                    None => {
                        let _ = send(
                            writer,
                            &ServerEvent::Error {
                                session: Some(session),
                                code: "no_session".into(),
                                message: "no such session".into(),
                            },
                        )
                        .await;
                        return;
                    }
                };
                let attach = match engine.attach(from_seq).await {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = send(
                            writer,
                            &ServerEvent::Error {
                                session: Some(session),
                                code: "attach_failed".into(),
                                message: e.to_string(),
                            },
                        )
                        .await;
                        return;
                    }
                };
                let _ = send(
                    writer,
                    &ServerEvent::Attached {
                        session: session.clone(),
                        seq_at_attach: attach.seq_at_attach,
                    },
                )
                .await;
                if let Some(prev) = subs.remove(&session) {
                    prev.abort();
                }
                let writer_for_pump = Arc::clone(writer);
                let session_for_pump = session.clone();
                let mut stream = attach.stream;
                let handle = tokio::spawn(async move {
                    while let Some(entry) = stream.next().await {
                        if send(
                            &writer_for_pump,
                            &ServerEvent::Frame {
                                session: session_for_pump.clone(),
                                entry,
                            },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                });
                subs.insert(session, handle);
            }

            ClientCommand::AcquireInput { session } => {
                let engine = match self.manager.get(&session).await {
                    Some(e) => e,
                    None => {
                        let _ = send(
                            writer,
                            &ServerEvent::Error {
                                session: Some(session),
                                code: "no_session".into(),
                                message: "no such session".into(),
                            },
                        )
                        .await;
                        return;
                    }
                };
                let acquired = match engine.try_acquire_input() {
                    Some(lease) => {
                        leases.insert(session.clone(), lease);
                        true
                    }
                    None => false,
                };
                let _ = send(writer, &ServerEvent::InputAcquired { session, acquired }).await;
            }

            ClientCommand::Send { session, text } => {
                let Some(lease) = leases.get(&session) else {
                    let _ = send(
                        writer,
                        &ServerEvent::Error {
                            session: Some(session),
                            code: "no_lease".into(),
                            message: "input lease not held by this connection".into(),
                        },
                    )
                    .await;
                    return;
                };
                if let Err(e) = lease.send(text) {
                    let _ = send(
                        writer,
                        &ServerEvent::Error {
                            session: Some(session),
                            code: "send_failed".into(),
                            message: e.to_string(),
                        },
                    )
                    .await;
                }
            }

            ClientCommand::ReleaseInput { session } => {
                leases.remove(&session);
                let _ = send(writer, &ServerEvent::InputReleased { session }).await;
            }

            ClientCommand::Detach { session } => {
                if let Some(h) = subs.remove(&session) {
                    h.abort();
                }
                let _ = send(writer, &ServerEvent::Detached { session }).await;
            }

            ClientCommand::Close { session } => {
                leases.remove(&session);
                if let Some(h) = subs.remove(&session) {
                    h.abort();
                }
                if let Err(e) = self.manager.close(&session).await {
                    let _ = send(
                        writer,
                        &ServerEvent::Error {
                            session: Some(session),
                            code: "close_failed".into(),
                            message: e.to_string(),
                        },
                    )
                    .await;
                } else {
                    let _ = send(writer, &ServerEvent::Closed { session }).await;
                }
            }

            ClientCommand::List => {
                let sessions = self.manager.list().await;
                let _ = send(writer, &ServerEvent::Listed { sessions }).await;
            }
        }
    }
}

async fn send<W>(writer: &Arc<Mutex<W>>, event: &ServerEvent) -> Result<()>
where
    W: AsyncWrite + Unpin + Send,
{
    let line = serde_json::to_string(event).map_err(|e| RoyError::Protocol(e.to_string()))?;
    let mut w = writer.lock().await;
    w.write_all(line.as_bytes()).await.map_err(RoyError::Io)?;
    w.write_all(b"\n").await.map_err(RoyError::Io)?;
    w.flush().await.map_err(RoyError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{StopReason, TurnEvent};
    use std::time::Duration;

    /// Test factory that ignores agent/model/permission and always builds the
    /// fake ACP agent.
    struct FakeAcpFactory;
    impl TransportFactory for FakeAcpFactory {
        fn build(
            &self,
            _agent: &str,
            _model: Option<&str>,
            _permission: Option<&str>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(AcpTransport::new(AcpConfig {
                command: "python3".to_string(),
                args: vec!["tests/scripts/fake-acp-agent.py".to_string()],
                mode_id: Some("yolo".to_string()),
                permission_policy: PermissionPolicy::AllowAll,
                open_timeout: Duration::from_secs(5),
            })))
        }
    }

    static TMPDIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("roy-daemon-test-{}-{n}", std::process::id()))
    }

    /// End-to-end through the daemon over an in-memory duplex pipe:
    /// spawn → attach → acquire_input → send → drain frames until Result.
    #[tokio::test]
    async fn spawn_attach_send_round_trip_over_duplex() {
        let dir = tmp_dir();
        let daemon = Arc::new(Daemon::new(dir.clone(), Arc::new(FakeAcpFactory)));

        let (client_side, server_side) = tokio::io::duplex(8192);
        let (server_rd, server_wr) = tokio::io::split(server_side);
        let serve_handle = {
            let d = Arc::clone(&daemon);
            tokio::spawn(async move {
                let _ = d.serve_connection(server_rd, server_wr).await;
            })
        };

        let (client_rd, mut client_wr) = tokio::io::split(client_side);
        let mut events = BufReader::new(client_rd).lines();

        async fn send_cmd<W: AsyncWrite + Unpin>(w: &mut W, cmd: &ClientCommand) {
            let line = serde_json::to_string(cmd).unwrap();
            w.write_all(line.as_bytes()).await.unwrap();
            w.write_all(b"\n").await.unwrap();
            w.flush().await.unwrap();
        }

        async fn next_event<R: AsyncRead + Unpin>(
            lines: &mut tokio::io::Lines<BufReader<R>>,
        ) -> ServerEvent {
            let line = lines.next_line().await.unwrap().expect("server hung up");
            serde_json::from_str(line.trim()).unwrap()
        }

        // 1. spawn
        send_cmd(
            &mut client_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                cwd: Some(std::env::current_dir().unwrap().to_string_lossy().into()),
                model: None,
                permission: None,
                resume: None,
            },
        )
        .await;
        let session = match next_event(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };

        // 2. attach
        send_cmd(
            &mut client_wr,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: None,
            },
        )
        .await;
        match next_event(&mut events).await {
            ServerEvent::Attached { .. } => {}
            other => panic!("expected Attached, got {other:?}"),
        }

        // 3. acquire input
        send_cmd(
            &mut client_wr,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        match next_event(&mut events).await {
            ServerEvent::InputAcquired { acquired: true, .. } => {}
            other => panic!("expected InputAcquired{{acquired:true}}, got {other:?}"),
        }

        // 4. send
        send_cmd(
            &mut client_wr,
            &ClientCommand::Send {
                session: session.clone(),
                text: "hello".into(),
            },
        )
        .await;

        // 5. drain Frame events until terminal Result
        let mut got_text = false;
        let mut got_result_end_turn = false;
        for _ in 0..32 {
            let ev = next_event(&mut events).await;
            if let ServerEvent::Frame { entry, .. } = ev {
                match entry.event {
                    TurnEvent::AssistantText { ref text } if text == "ack" => got_text = true,
                    TurnEvent::Result {
                        stop_reason: StopReason::EndTurn,
                        ..
                    } => {
                        got_result_end_turn = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        assert!(got_text, "expected an 'ack' AssistantText frame");
        assert!(got_result_end_turn, "expected a terminal Result{{EndTurn}}");

        // 6. close
        send_cmd(
            &mut client_wr,
            &ClientCommand::Close {
                session: session.clone(),
            },
        )
        .await;
        match next_event(&mut events).await {
            ServerEvent::Closed { .. } => {}
            other => panic!("expected Closed, got {other:?}"),
        }

        // Hang up; the server task should finish cleanly.
        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;

        let _ = std::fs::remove_dir_all(&dir);
    }
}
