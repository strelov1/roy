//! `roy serve` daemon: owns one `SessionManager` and serves connections from
//! triggers (Unix socket and WebSocket today, more later) speaking the control
//! protocol defined in `crate::control`.
//!
//! Wire format is the same JSON payload on both transports. Each transport
//! gets its own writer task that drains a per-connection `mpsc<ServerEvent>`
//! and serializes events to its native framing — `\n`-delimited bytes on Unix
//! socket, `Message::Text` on WebSocket. The command-dispatch loop is shared.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::control::{ClientCommand, ServerEvent};
use crate::engine::{InputLease, SessionSpawnConfig};
use crate::error::{Result, RoyError};
use crate::manager::SessionManager;
use crate::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};

/// One queued event for the writer task.
type EventTx = mpsc::UnboundedSender<ServerEvent>;

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

/// Options bundle for `Daemon::run_with_opts` — knobs the CLI exposes via
/// `roy serve` flags. Construct via `Default::default()` and override only
/// what you need.
#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub socket_path: PathBuf,
    pub ws_port: Option<u16>,
    /// Auto-close any session quiet past this threshold. `None` disables GC.
    pub idle_timeout: Option<std::time::Duration>,
    /// Resurrect every archived session in `journal_dir` at startup.
    pub resume_all: bool,
}

/// The daemon. Holds the shared manager and the transport factory; you can
/// drive it over a Unix listener (`run_unix`), a TCP-WebSocket listener
/// (`run_ws`), or pump a single connection by hand (`serve_connection` /
/// `serve_ws_connection`, useful in tests). High-level entry point is
/// `run_with_opts`.
pub struct Daemon {
    pub manager: Arc<SessionManager>,
}

impl Daemon {
    pub fn new(journal_dir: PathBuf, factory: Arc<dyn TransportFactory>) -> Self {
        Self {
            manager: Arc::new(SessionManager::new(journal_dir, factory)),
        }
    }

    /// High-level entry: resume-all (if requested), spawn the idle-GC task
    /// (if configured), then run the Unix listener and optionally the WS
    /// listener concurrently. Returns whichever side errors first; on
    /// graceful shutdown the calling process exits.
    pub async fn run_with_opts(self: Arc<Self>, opts: ServeOpts) -> Result<()> {
        if opts.resume_all {
            let results = self.manager.resume_all(256, 1024).await;
            for (id, err) in &results {
                match err {
                    None => eprintln!("roy: resumed {id}"),
                    Some(e) => eprintln!("roy: failed to resume {id}: {e}"),
                }
            }
        }
        if let Some(threshold) = opts.idle_timeout {
            let mgr = Arc::clone(&self.manager);
            let tick = std::cmp::max(threshold / 4, std::time::Duration::from_millis(50));
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                interval.set_missed_tick_behavior(
                    tokio::time::MissedTickBehavior::Delay,
                );
                loop {
                    interval.tick().await;
                    let closed = mgr.sweep_idle(threshold).await;
                    for id in closed {
                        eprintln!("roy: closed idle session {id}");
                    }
                }
            });
        }

        let unix = {
            let me = Arc::clone(&self);
            let path = opts.socket_path.clone();
            tokio::spawn(async move { me.run_unix(&path).await })
        };
        let ws = opts.ws_port.map(|port| {
            let me = Arc::clone(&self);
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("valid addr");
            tokio::spawn(async move { me.run_ws(addr).await })
        });

        match ws {
            Some(ws_handle) => {
                let (u, w) = tokio::join!(unix, ws_handle);
                u.map_err(|e| RoyError::Protocol(e.to_string()))??;
                w.map_err(|e| RoyError::Protocol(e.to_string()))??;
                Ok(())
            }
            None => unix
                .await
                .map_err(|e| RoyError::Protocol(e.to_string()))?
                .map(|_| ()),
        }
    }

    /// Listen on a Unix socket, accept connections forever. Refuses to start
    /// if another roy daemon already owns `<socket_path>.pid`.
    pub async fn run_unix(self: Arc<Self>, socket_path: &Path) -> Result<()> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(RoyError::Io)?;
        }
        // PID-file lock first: this is the single-instance gate. If it
        // succeeds, any leftover socket file is necessarily stale (the prior
        // owner is dead by the liveness check inside `PidLock::acquire`).
        let pid_path = socket_path.with_extension(
            socket_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("{e}.pid"))
                .unwrap_or_else(|| "pid".to_string()),
        );
        let _pid_lock = crate::pid_lock::PidLock::acquire(&pid_path)?;
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

    /// Listen for incoming WebSocket connections on `addr` (e.g. "127.0.0.1:7777").
    pub async fn run_ws(self: Arc<Self>, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await.map_err(RoyError::Io)?;
        loop {
            let (stream, _) = listener.accept().await.map_err(RoyError::Io)?;
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                let ws = match tokio_tungstenite::accept_async(stream).await {
                    Ok(ws) => ws,
                    Err(_) => return,
                };
                let _ = me.serve_ws_connection(ws).await;
            });
        }
    }

    /// Drive one byte-stream client connection (Unix socket or duplex test).
    pub async fn serve_connection<R, W>(self: &Arc<Self>, reader: R, writer: W) -> Result<()>
    where
        R: AsyncRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let writer_handle = tokio::spawn(line_writer_loop(writer, event_rx));
        let result = self.dispatch_lines(reader, event_tx).await;
        // event_tx dropped → writer_loop sees None → exits cleanly.
        let _ = writer_handle.await;
        result
    }

    /// Drive one WebSocket client connection.
    pub async fn serve_ws_connection<S>(
        self: &Arc<Self>,
        ws: WebSocketStream<S>,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let (ws_sink, ws_stream) = ws.split();
        let writer_handle = tokio::spawn(ws_writer_loop(ws_sink, event_rx));
        let result = self.dispatch_ws(ws_stream, event_tx).await;
        let _ = writer_handle.await;
        result
    }

    async fn dispatch_lines<R>(self: &Arc<Self>, reader: R, event_tx: EventTx) -> Result<()>
    where
        R: AsyncRead + Unpin + Send,
    {
        let mut lines = BufReader::new(reader).lines();
        let mut subs: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
        let mut leases: HashMap<String, InputLease> = HashMap::new();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            self.dispatch_one_command(line, &event_tx, &mut subs, &mut leases)
                .await;
        }

        for handle in subs.into_values() {
            handle.abort();
        }
        Ok(())
    }

    async fn dispatch_ws<S>(
        self: &Arc<Self>,
        mut stream: futures_util::stream::SplitStream<WebSocketStream<S>>,
        event_tx: EventTx,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut subs: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
        let mut leases: HashMap<String, InputLease> = HashMap::new();

        while let Some(msg) = stream.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => break,
                // Ignore binary / ping / pong frames; tungstenite handles
                // ping/pong itself.
                _ => continue,
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            self.dispatch_one_command(text, &event_tx, &mut subs, &mut leases)
                .await;
        }

        for handle in subs.into_values() {
            handle.abort();
        }
        Ok(())
    }

    async fn dispatch_one_command(
        self: &Arc<Self>,
        text: &str,
        event_tx: &EventTx,
        subs: &mut HashMap<String, tokio::task::JoinHandle<()>>,
        leases: &mut HashMap<String, InputLease>,
    ) {
        let cmd: ClientCommand = match serde_json::from_str(text) {
            Ok(c) => c,
            Err(e) => {
                let _ = event_tx.send(ServerEvent::Error {
                    session: None,
                    code: "bad_request".into(),
                    message: e.to_string(),
                });
                return;
            }
        };
        self.handle(cmd, event_tx, subs, leases).await;
    }

    async fn handle(
        self: &Arc<Self>,
        cmd: ClientCommand,
        event_tx: &EventTx,
        subs: &mut HashMap<String, tokio::task::JoinHandle<()>>,
        leases: &mut HashMap<String, InputLease>,
    ) {
        match cmd {
            ClientCommand::Spawn {
                agent,
                cwd,
                model,
                permission,
                resume,
            } => {
                let cfg = SessionSpawnConfig {
                    agent,
                    cwd: cwd.map(PathBuf::from).unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    }),
                    model,
                    permission,
                    resume_cursor: resume,
                };
                let engine = match self.manager.spawn(cfg, 256, 1024).await {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::Error {
                            session: None,
                            code: "spawn_failed".into(),
                            message: e.to_string(),
                        });
                        return;
                    }
                };
                let session = engine.id().to_string();
                let resume_cursor = engine.resume_cursor().await;
                let _ = event_tx.send(ServerEvent::Spawned {
                    session,
                    resume_cursor,
                });
            }

            ClientCommand::Resume { session } => {
                let engine = match self.manager.resume(&session, 256, 1024).await {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::Error {
                            session: Some(session),
                            code: "resume_failed".into(),
                            message: e.to_string(),
                        });
                        return;
                    }
                };
                let resume_cursor = engine.resume_cursor().await;
                let _ = event_tx.send(ServerEvent::Resumed {
                    session: engine.id().to_string(),
                    resume_cursor,
                });
            }

            ClientCommand::Attach { session, from_seq } => {
                let engine = match self.manager.get(&session).await {
                    Some(e) => e,
                    None => {
                        // Fall back to a read-only archive replay if the
                        // journal still exists. No live broadcast — the
                        // pump task emits the disk snapshot and ends.
                        match self.manager.open_archive(&session).await {
                            Ok(archive) => {
                                let entries =
                                    match archive.replay_from(from_seq.unwrap_or(0)).await {
                                        Ok(e) => e,
                                        Err(e) => {
                                            let _ = event_tx.send(ServerEvent::Error {
                                                session: Some(session),
                                                code: "archive_read_failed".into(),
                                                message: e.to_string(),
                                            });
                                            return;
                                        }
                                    };
                                let seq_at_attach = entries
                                    .last()
                                    .map(|e| e.seq + 1)
                                    .unwrap_or_else(|| from_seq.unwrap_or(0));
                                let _ = event_tx.send(ServerEvent::Attached {
                                    session: session.clone(),
                                    seq_at_attach,
                                });
                                let tx = event_tx.clone();
                                let sid = session.clone();
                                let handle = tokio::spawn(async move {
                                    for entry in entries {
                                        if tx
                                            .send(ServerEvent::Frame {
                                                session: sid.clone(),
                                                entry,
                                            })
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                });
                                if let Some(prev) = subs.remove(&session) {
                                    prev.abort();
                                }
                                subs.insert(session, handle);
                            }
                            Err(_) => {
                                let _ = event_tx.send(ServerEvent::Error {
                                    session: Some(session),
                                    code: "no_session".into(),
                                    message: "no such session (live or archived)".into(),
                                });
                            }
                        }
                        return;
                    }
                };
                let attach = match engine.attach(from_seq).await {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::Error {
                            session: Some(session),
                            code: "attach_failed".into(),
                            message: e.to_string(),
                        });
                        return;
                    }
                };
                let _ = event_tx.send(ServerEvent::Attached {
                    session: session.clone(),
                    seq_at_attach: attach.seq_at_attach,
                });
                if let Some(prev) = subs.remove(&session) {
                    prev.abort();
                }
                let event_tx_for_pump = event_tx.clone();
                let session_for_pump = session.clone();
                let mut stream = attach.stream;
                let handle = tokio::spawn(async move {
                    while let Some(entry) = stream.next().await {
                        if event_tx_for_pump
                            .send(ServerEvent::Frame {
                                session: session_for_pump.clone(),
                                entry,
                            })
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
                        let _ = event_tx.send(ServerEvent::Error {
                            session: Some(session),
                            code: "no_session".into(),
                            message: "no such session".into(),
                        });
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
                let _ = event_tx.send(ServerEvent::InputAcquired { session, acquired });
            }

            ClientCommand::Send { session, text } => {
                let Some(lease) = leases.get(&session) else {
                    let _ = event_tx.send(ServerEvent::Error {
                        session: Some(session),
                        code: "no_lease".into(),
                        message: "input lease not held by this connection".into(),
                    });
                    return;
                };
                if let Err(e) = lease.send(text) {
                    let _ = event_tx.send(ServerEvent::Error {
                        session: Some(session),
                        code: "send_failed".into(),
                        message: e.to_string(),
                    });
                }
            }

            ClientCommand::ReleaseInput { session } => {
                leases.remove(&session);
                let _ = event_tx.send(ServerEvent::InputReleased { session });
            }

            ClientCommand::Detach { session } => {
                if let Some(h) = subs.remove(&session) {
                    h.abort();
                }
                let _ = event_tx.send(ServerEvent::Detached { session });
            }

            ClientCommand::Close { session } => {
                leases.remove(&session);
                if let Some(h) = subs.remove(&session) {
                    h.abort();
                }
                if let Err(e) = self.manager.close(&session).await {
                    let _ = event_tx.send(ServerEvent::Error {
                        session: Some(session),
                        code: "close_failed".into(),
                        message: e.to_string(),
                    });
                } else {
                    let _ = event_tx.send(ServerEvent::Closed { session });
                }
            }

            ClientCommand::List => {
                let sessions = self.manager.list().await;
                let _ = event_tx.send(ServerEvent::Listed { sessions });
            }

            ClientCommand::ReadJournal {
                session,
                from_seq,
                max_entries,
            } => {
                let from = from_seq.unwrap_or(0);
                // Live session takes precedence; fall back to archive on miss.
                let read_result = if let Some(engine) = self.manager.get(&session).await {
                    engine.snapshot(from).await
                } else {
                    match self.manager.open_archive(&session).await {
                        Ok(archive) => archive.replay_from(from).await,
                        Err(e) => Err(e),
                    }
                };
                match read_result {
                    Ok(mut entries) => {
                        let has_more = max_entries
                            .map(|n| entries.len() > n)
                            .unwrap_or(false);
                        if let Some(n) = max_entries {
                            entries.truncate(n);
                        }
                        let next_seq =
                            entries.last().map(|e| e.seq + 1).unwrap_or(from);
                        let _ = event_tx.send(ServerEvent::JournalRead {
                            session,
                            entries,
                            next_seq,
                            has_more,
                        });
                    }
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::Error {
                            session: Some(session),
                            code: "read_journal_failed".into(),
                            message: e.to_string(),
                        });
                    }
                }
            }

            ClientCommand::ListArchived => {
                match self.manager.list_archived().await {
                    Ok(sessions) => {
                        let _ = event_tx.send(ServerEvent::ListedArchived { sessions });
                    }
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::Error {
                            session: None,
                            code: "list_archived_failed".into(),
                            message: e.to_string(),
                        });
                    }
                }
            }
        }
    }
}

async fn line_writer_loop<W>(mut writer: W, mut rx: mpsc::UnboundedReceiver<ServerEvent>)
where
    W: AsyncWrite + Unpin,
{
    while let Some(event) = rx.recv().await {
        let json = match serde_json::to_string(&event) {
            Ok(j) => j,
            Err(_) => continue,
        };
        if writer.write_all(json.as_bytes()).await.is_err() {
            break;
        }
        if writer.write_all(b"\n").await.is_err() {
            break;
        }
        if writer.flush().await.is_err() {
            break;
        }
    }
}

async fn ws_writer_loop<S>(
    mut sink: futures_util::stream::SplitSink<WebSocketStream<S>, Message>,
    mut rx: mpsc::UnboundedReceiver<ServerEvent>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(event) = rx.recv().await {
        let json = match serde_json::to_string(&event) {
            Ok(j) => j,
            Err(_) => continue,
        };
        if sink.send(Message::Text(json.into())).await.is_err() {
            break;
        }
    }
    let _ = sink.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{StopReason, TurnEvent};
    use std::time::Duration;

    /// Test factory: ignores agent/model/permission and always builds the fake
    /// ACP agent.
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

    async fn send_cmd_line<W: AsyncWrite + Unpin>(w: &mut W, cmd: &ClientCommand) {
        let line = serde_json::to_string(cmd).unwrap();
        w.write_all(line.as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        w.flush().await.unwrap();
    }

    async fn next_event_line<R: AsyncRead + Unpin>(
        lines: &mut tokio::io::Lines<BufReader<R>>,
    ) -> ServerEvent {
        let line = lines.next_line().await.unwrap().expect("server hung up");
        serde_json::from_str(line.trim()).unwrap()
    }

    /// End-to-end through the daemon over an in-memory duplex pipe.
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

        send_cmd_line(
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
        let session = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };

        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: None,
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Attached { .. } => {}
            other => panic!("expected Attached, got {other:?}"),
        }

        send_cmd_line(
            &mut client_wr,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::InputAcquired { acquired: true, .. } => {}
            other => panic!("expected InputAcquired{{acquired:true}}, got {other:?}"),
        }

        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Send {
                session: session.clone(),
                text: "hello".into(),
            },
        )
        .await;

        let mut got_text = false;
        let mut got_result_end_turn = false;
        for _ in 0..32 {
            let ev = next_event_line(&mut events).await;
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

        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Close {
                session: session.clone(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Closed { .. } => {}
            other => panic!("expected Closed, got {other:?}"),
        }

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After a session is closed, `Attach` to its id must fall back to the
    /// on-disk journal (read-only replay), and `ListArchived` must include it
    /// while live `List` does not.
    #[tokio::test]
    async fn closed_session_is_attachable_via_archive_fallback() {
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

        // Spawn → drive one turn → close.
        send_cmd_line(
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
        let session = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: None,
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Attached
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // InputAcquired
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Send {
                session: session.clone(),
                text: "hello".into(),
            },
        )
        .await;
        // Drain until Result.
        loop {
            if let ServerEvent::Frame { entry, .. } = next_event_line(&mut events).await {
                if matches!(entry.event, TurnEvent::Result { .. }) {
                    break;
                }
            }
        }
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Close {
                session: session.clone(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Closed { .. } => {}
            other => panic!("expected Closed, got {other:?}"),
        }

        // Live list is empty; archived list contains the closed session.
        send_cmd_line(&mut client_wr, &ClientCommand::List).await;
        match next_event_line(&mut events).await {
            ServerEvent::Listed { sessions } => assert!(sessions.is_empty()),
            other => panic!("expected Listed, got {other:?}"),
        }
        send_cmd_line(&mut client_wr, &ClientCommand::ListArchived).await;
        match next_event_line(&mut events).await {
            ServerEvent::ListedArchived { sessions } => {
                assert!(sessions.contains(&session), "archive list missing closed session");
            }
            other => panic!("expected ListedArchived, got {other:?}"),
        }

        // Attach to the closed session must fall back to the archive and
        // replay the journal until the terminal Result.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: None,
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Attached { .. } => {}
            other => panic!("expected Attached on archive replay, got {other:?}"),
        }
        let mut saw_result = false;
        for _ in 0..32 {
            match next_event_line(&mut events).await {
                ServerEvent::Frame { entry, .. } => {
                    if matches!(entry.event, TurnEvent::Result { .. }) {
                        saw_result = true;
                        break;
                    }
                }
                other => panic!("expected Frame, got {other:?}"),
            }
        }
        assert!(saw_result, "archive replay must include the terminal Result");

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ReadJournal` returns a snapshot of the journal for a live session,
    /// honours `from_seq` / `max_entries`, and reports `has_more` when
    /// truncated.
    #[tokio::test]
    async fn read_journal_snapshot_paginates_a_live_session() {
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

        // Spawn + drive one full turn so the journal has several entries.
        send_cmd_line(
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
        let session = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: None,
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Attached
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // InputAcquired
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Send {
                session: session.clone(),
                text: "hello".into(),
            },
        )
        .await;
        let mut total = 0;
        loop {
            if let ServerEvent::Frame { entry, .. } = next_event_line(&mut events).await {
                total += 1;
                if matches!(entry.event, TurnEvent::Result { .. }) {
                    break;
                }
            }
        }
        assert!(total >= 2, "fake should emit at least one chunk + result");

        // 1. ReadJournal from 0, no max — returns the whole journal.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::ReadJournal {
                session: session.clone(),
                from_seq: None,
                max_entries: None,
            },
        )
        .await;
        let (all_entries, next_seq, has_more) = match next_event_line(&mut events).await {
            ServerEvent::JournalRead {
                entries,
                next_seq,
                has_more,
                ..
            } => (entries, next_seq, has_more),
            other => panic!("expected JournalRead, got {other:?}"),
        };
        assert_eq!(all_entries.len(), total);
        assert!(!has_more);
        assert_eq!(next_seq, total as u64);

        // 2. max_entries=1 truncates and sets has_more.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::ReadJournal {
                session: session.clone(),
                from_seq: Some(0),
                max_entries: Some(1),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::JournalRead {
                entries,
                next_seq,
                has_more,
                ..
            } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].seq, 0);
                assert_eq!(next_seq, 1);
                assert!(has_more, "max_entries truncation must set has_more");
            }
            other => panic!("expected JournalRead, got {other:?}"),
        }

        // 3. from_seq past the end returns empty slice with next_seq == from_seq.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::ReadJournal {
                session: session.clone(),
                from_seq: Some(total as u64),
                max_entries: None,
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::JournalRead {
                entries,
                next_seq,
                has_more,
                ..
            } => {
                assert!(entries.is_empty());
                assert_eq!(next_seq, total as u64);
                assert!(!has_more);
            }
            other => panic!("expected JournalRead, got {other:?}"),
        }

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Full live-session resurrection cycle: spawn → drive a turn → close →
    /// `ClientCommand::Resume { session }` → drive another turn → attach to
    /// see the full journal with monotonic seqs across the gap.
    #[tokio::test]
    async fn close_then_resume_continues_the_journal() {
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

        // Helper: drive one turn and collect seqs of the resulting frames.
        async fn drive_turn(
            client_wr: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
            events: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
            session: &str,
            text: &str,
        ) -> Vec<u64> {
            send_cmd_line(
                client_wr,
                &ClientCommand::Send {
                    session: session.into(),
                    text: text.into(),
                },
            )
            .await;
            let mut seqs = Vec::new();
            loop {
                if let ServerEvent::Frame { entry, .. } = next_event_line(events).await {
                    seqs.push(entry.seq);
                    if matches!(entry.event, TurnEvent::Result { .. }) {
                        break;
                    }
                }
            }
            seqs
        }

        // 1. Spawn fresh, attach, acquire, drive turn 1, close.
        send_cmd_line(
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
        let session = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: None,
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Attached
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // InputAcquired
        let turn1_seqs = drive_turn(&mut client_wr, &mut events, &session, "first").await;
        assert!(!turn1_seqs.is_empty());
        let last_turn1 = *turn1_seqs.last().unwrap();

        // Detach + release input so close doesn't fight a live lease.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::ReleaseInput {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await;
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Detach {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await;
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Close {
                session: session.clone(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Closed { .. } => {}
            other => panic!("expected Closed, got {other:?}"),
        }

        // 2. Resume.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Resume {
                session: session.clone(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Resumed {
                session: resumed_id,
                ..
            } => assert_eq!(resumed_id, session, "resume must keep the same session id"),
            other => panic!("expected Resumed, got {other:?}"),
        }

        // 3. Attach + acquire + drive turn 2.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: Some(last_turn1 + 1),
            },
        )
        .await;
        let attached_seq = match next_event_line(&mut events).await {
            ServerEvent::Attached { seq_at_attach, .. } => seq_at_attach,
            other => panic!("expected Attached, got {other:?}"),
        };
        // attached_seq should be > last_turn1 — the journal continues, not restarts.
        assert!(
            attached_seq >= last_turn1 + 1,
            "resumed journal must continue past last_turn1={last_turn1}, got attached_seq={attached_seq}"
        );
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await;
        let turn2_seqs = drive_turn(&mut client_wr, &mut events, &session, "second").await;
        assert!(!turn2_seqs.is_empty());
        // Monotonic across the gap.
        let first_turn2 = *turn2_seqs.first().unwrap();
        assert!(
            first_turn2 > last_turn1,
            "turn2 seqs must continue past turn1; last_turn1={last_turn1}, first_turn2={first_turn2}"
        );

        // 4. Cleanup.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Close {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await;
        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Resume goes through to the transport: a Spawn with `resume = Some(sid)`
    /// must use ACP `session/load` and the resulting `Spawned.resume_cursor`
    /// must be the supplied `sid` (not a fresh one from `session/new`).
    #[tokio::test]
    async fn spawn_with_resume_uses_session_load() {
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

        // Fresh session → fake's session/new returns "fake-acp-sid".
        send_cmd_line(
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
        let fresh_cursor = match next_event_line(&mut events).await {
            ServerEvent::Spawned { resume_cursor, .. } => resume_cursor,
            other => panic!("expected Spawned, got {other:?}"),
        };
        assert_eq!(fresh_cursor.as_deref(), Some("fake-acp-sid"));

        // Resume → AcpTransport routes through session/load and keeps the
        // supplied sid as the cursor.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                cwd: Some(std::env::current_dir().unwrap().to_string_lossy().into()),
                model: None,
                permission: None,
                resume: Some("prior-session-sid".into()),
            },
        )
        .await;
        let resumed_cursor = match next_event_line(&mut events).await {
            ServerEvent::Spawned { resume_cursor, .. } => resume_cursor,
            other => panic!("expected Spawned, got {other:?}"),
        };
        assert_eq!(resumed_cursor.as_deref(), Some("prior-session-sid"));

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end through the daemon over a real TCP WebSocket. Validates
    /// that the same control protocol works over WS framing.
    #[tokio::test]
    async fn spawn_attach_send_round_trip_over_websocket() {
        let dir = tmp_dir();
        let daemon = Arc::new(Daemon::new(dir.clone(), Arc::new(FakeAcpFactory)));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = {
            let d = Arc::clone(&daemon);
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let _ = d.serve_ws_connection(ws).await;
            })
        };

        let url = format!("ws://{addr}");
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        async fn ws_send(
            ws: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            cmd: &ClientCommand,
        ) {
            let json = serde_json::to_string(cmd).unwrap();
            ws.send(Message::Text(json.into())).await.unwrap();
        }
        async fn ws_recv(
            ws: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        ) -> ServerEvent {
            loop {
                let msg = ws.next().await.expect("ws closed").unwrap();
                if let Message::Text(text) = msg {
                    return serde_json::from_str(text.as_str()).unwrap();
                }
            }
        }

        ws_send(
            &mut ws,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                cwd: Some(std::env::current_dir().unwrap().to_string_lossy().into()),
                model: None,
                permission: None,
                resume: None,
            },
        )
        .await;
        let session = match ws_recv(&mut ws).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };

        ws_send(
            &mut ws,
            &ClientCommand::Attach {
                session: session.clone(),
                from_seq: None,
            },
        )
        .await;
        match ws_recv(&mut ws).await {
            ServerEvent::Attached { .. } => {}
            other => panic!("expected Attached, got {other:?}"),
        }

        ws_send(
            &mut ws,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        match ws_recv(&mut ws).await {
            ServerEvent::InputAcquired { acquired: true, .. } => {}
            other => panic!("expected InputAcquired, got {other:?}"),
        }

        ws_send(
            &mut ws,
            &ClientCommand::Send {
                session: session.clone(),
                text: "hello".into(),
            },
        )
        .await;

        let mut got_end_turn = false;
        for _ in 0..32 {
            if let ServerEvent::Frame { entry, .. } = ws_recv(&mut ws).await {
                if matches!(
                    entry.event,
                    TurnEvent::Result {
                        stop_reason: StopReason::EndTurn,
                        ..
                    }
                ) {
                    got_end_turn = true;
                    break;
                }
            }
        }
        assert!(got_end_turn, "expected terminal Result{{EndTurn}} over WS");

        ws_send(
            &mut ws,
            &ClientCommand::Close {
                session: session.clone(),
            },
        )
        .await;
        match ws_recv(&mut ws).await {
            ServerEvent::Closed { .. } => {}
            other => panic!("expected Closed, got {other:?}"),
        }

        let _ = ws.close(None).await;
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
