//! `roy serve` daemon: owns one `SessionManager` and serves connections from
//! triggers (Unix socket and WebSocket today, more later) speaking the control
//! protocol defined in `crate::control`.
//!
//! Wire format is the same JSON payload on both transports. Each transport
//! gets its own writer task that drains a per-connection `mpsc<ServerEvent>`
//! and serializes events to its native framing — `\n`-delimited bytes on Unix
//! socket, `Message::Text` on WebSocket. The command-dispatch loop is shared.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::agents_config::AgentPreset;
use crate::control::{ClientCommand, ErrorCode, FireTarget, ServerEvent};
use crate::engine::{InputLease, SessionSpawnConfig};
use crate::error::{Result, RoyError};
use crate::journal::Seq;
use crate::manager::SessionManager;
use crate::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};

/// One queued event for the writer task.
type EventTx = mpsc::UnboundedSender<ServerEvent>;

/// Per-connection live attach pumps, keyed by session id.
type SubsMap = HashMap<String, tokio::task::JoinHandle<()>>;

/// Per-connection input leases, keyed by session id.
type LeasesMap = HashMap<String, InputLease>;

/// Browsers can't set arbitrary headers on `new WebSocket(url, [protocols])`,
/// so the auth token rides the subprotocol slot instead.
const WS_TOKEN_HEADER: &str = "sec-websocket-protocol";

/// Load the WS auth token from `<socket>.token` or mint a fresh UUID and write
/// it. File is owner-only (`0600`); see [`crate::pid_lock::create_owner_only_file`].
pub fn load_or_create_ws_token(token_path: &Path) -> Result<String> {
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent).map_err(RoyError::Io)?;
    }
    match std::fs::read_to_string(token_path) {
        Ok(s) => Ok(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = uuid::Uuid::new_v4().to_string();
            crate::pid_lock::create_owner_only_file(token_path, token.as_bytes())
                .map_err(RoyError::Io)?;
            Ok(token)
        }
        Err(e) => Err(RoyError::Io(e)),
    }
}

fn ws_auth_callback(
    expected: Arc<String>,
) -> impl FnOnce(&Request, Response) -> std::result::Result<Response, ErrorResponse> {
    move |req, mut resp| {
        let provided = req
            .headers()
            .get(WS_TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected.as_str() {
            let body = if provided.is_empty() {
                "missing roy ws token (set Sec-WebSocket-Protocol)"
            } else {
                "invalid roy ws token"
            };
            return Err(http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(Some(body.into()))
                .expect("valid http response"));
        }
        // WS spec: when the server selects a subprotocol it must echo it back,
        // or the browser's handshake fails.
        resp.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_str(provided).expect("token is ascii uuid"),
        );
        Ok(resp)
    }
}

/// Tiny helper to factor away the repetitive error-event send.
fn send_error(tx: &EventTx, session: Option<String>, code: ErrorCode, message: impl Into<String>) {
    let _ = tx.send(ServerEvent::Error {
        session,
        code,
        message: message.into(),
    });
}

/// How the daemon builds a `Transport` from an agent preset. Pluggable so the
/// daemon can be tested against fake agents without touching global state.
pub trait TransportFactory: Send + Sync {
    fn build(
        &self,
        agent: AgentPreset,
        model: Option<&str>,
        permission: Option<&str>,
    ) -> Result<Arc<dyn Transport>>;
}

/// Default mapping `agent name → AcpConfig` for the four ACP presets.
pub struct DefaultTransportFactory;

impl TransportFactory for DefaultTransportFactory {
    fn build(
        &self,
        agent: AgentPreset,
        _model: Option<&str>,
        permission: Option<&str>,
    ) -> Result<Arc<dyn Transport>> {
        let mut config = match agent {
            AgentPreset::Claude => AcpConfig::claude(),
            AgentPreset::Gemini => AcpConfig::gemini(),
            AgentPreset::Opencode => AcpConfig::opencode(),
            AgentPreset::Codex => AcpConfig::codex(),
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
    pub fn new(
        journal_dir: PathBuf,
        workspace_dir: PathBuf,
        factory: Arc<dyn TransportFactory>,
    ) -> Result<Self> {
        Ok(Self {
            manager: Arc::new(SessionManager::new(journal_dir, workspace_dir, factory)?),
        })
    }

    /// High-level entry: resume-all (if requested), spawn the idle-GC task
    /// (if configured), then run the Unix listener and optionally the WS
    /// listener concurrently. Returns whichever side errors first; on
    /// graceful shutdown the calling process exits.
    pub async fn run_with_opts(self: Arc<Self>, opts: ServeOpts) -> Result<()> {
        if opts.resume_all {
            tracing::info!("resume-all: scanning archives");
            let results = self.manager.resume_all(256, 1024).await;
            for (id, err) in &results {
                match err {
                    None => tracing::info!(session = %id, "resumed"),
                    Some(e) => tracing::warn!(session = %id, error = %e, "resume failed"),
                }
            }
        }
        self.manager.index_existing_sessions().await?;
        if let Some(threshold) = opts.idle_timeout {
            let mgr = Arc::clone(&self.manager);
            let tick = std::cmp::max(threshold / 4, std::time::Duration::from_millis(50));
            tracing::info!(?threshold, ?tick, "idle-sweep enabled");
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    let closed = mgr.sweep_idle(threshold).await;
                    for id in closed {
                        tracing::info!(session = %id, "closed idle session");
                    }
                }
            });
        }

        let unix = {
            let me = Arc::clone(&self);
            let path = opts.socket_path.clone();
            tokio::spawn(async move { me.run_unix(&path).await })
        };
        let ws = if let Some(port) = opts.ws_port {
            // Load (or create) the shared-secret token clients must present
            // during the WS handshake. Stored at `<socket>.token`, mode 0600.
            let token_path = opts.socket_path.with_extension("token");
            let token = Arc::new(load_or_create_ws_token(&token_path)?);
            tracing::info!(path = %token_path.display(), "ws auth token");
            let me = Arc::clone(&self);
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("valid addr");
            Some(tokio::spawn(async move { me.run_ws(addr, token).await }))
        } else {
            None
        };

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
            // Lock the parent dir down: the daemon owns it, and any sibling
            // file (sockets, pid files) carries control authority.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        // PID-file lock first: this is the single-instance gate. If it
        // succeeds, any leftover socket file is necessarily stale (the prior
        // owner is dead by the liveness check inside `PidLock::acquire`).
        let pid_path = crate::pid_lock::pid_path_for_socket(socket_path);
        let _pid_lock = crate::pid_lock::PidLock::acquire(&pid_path)?;
        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path).map_err(RoyError::Io)?;
        // Restrict socket to owner — connecting to it gives full control-
        // protocol access (spawn agents, read journals). With the default
        // umask 022 the socket would be world-connectable on Linux.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
                .map_err(RoyError::Io)?;
        }
        tracing::info!(path = %socket_path.display(), "unix listener up");

        loop {
            let (stream, _) = listener.accept().await.map_err(RoyError::Io)?;
            tracing::debug!("unix connection accepted");
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                let (reader, writer) = stream.into_split();
                if let Err(e) = me.serve_connection(reader, writer).await {
                    tracing::warn!(error = %e, "unix connection ended with error");
                }
            });
        }
    }

    /// Accept WS connections on `addr`. Clients must present `token` via
    /// [`WS_TOKEN_HEADER`]; the upgrade returns HTTP 401 otherwise.
    pub async fn run_ws(self: Arc<Self>, addr: SocketAddr, token: Arc<String>) -> Result<()> {
        let listener = TcpListener::bind(addr).await.map_err(RoyError::Io)?;
        tracing::info!(%addr, "websocket listener up");
        loop {
            let (stream, peer) = listener.accept().await.map_err(RoyError::Io)?;
            let me = Arc::clone(&self);
            let token_for_conn = Arc::clone(&token);
            tokio::spawn(async move {
                let callback = ws_auth_callback(token_for_conn);
                let ws = match tokio_tungstenite::accept_hdr_async(stream, callback).await {
                    Ok(ws) => ws,
                    Err(e) => {
                        tracing::warn!(%peer, error = %e, "ws handshake rejected");
                        return;
                    }
                };
                tracing::debug!(%peer, "ws connection accepted");
                if let Err(e) = me.serve_ws_connection(ws).await {
                    tracing::warn!(%peer, error = %e, "ws connection ended with error");
                }
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
        log_writer_join(writer_handle.await);
        result
    }

    /// Drive one WebSocket client connection.
    pub async fn serve_ws_connection<S>(self: &Arc<Self>, ws: WebSocketStream<S>) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let (ws_sink, ws_stream) = ws.split();
        let writer_handle = tokio::spawn(ws_writer_loop(ws_sink, event_rx));
        let result = self.dispatch_ws(ws_stream, event_tx).await;
        log_writer_join(writer_handle.await);
        result
    }

    async fn dispatch_lines<R>(self: &Arc<Self>, reader: R, event_tx: EventTx) -> Result<()>
    where
        R: AsyncRead + Unpin + Send,
    {
        let mut lines = BufReader::new(reader).lines();
        let mut subs: SubsMap = HashMap::new();
        let mut leases: LeasesMap = HashMap::new();

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
        let mut subs: SubsMap = HashMap::new();
        let mut leases: LeasesMap = HashMap::new();

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
        subs: &mut SubsMap,
        leases: &mut LeasesMap,
    ) {
        let cmd: ClientCommand = match serde_json::from_str(text) {
            Ok(c) => c,
            Err(e) => {
                send_error(event_tx, None, ErrorCode::BadRequest, e.to_string());
                return;
            }
        };
        self.handle(cmd, event_tx, subs, leases).await;
    }

    /// Thin command dispatcher: every non-trivial branch lives in its own
    /// `handle_*` method so per-command logic is easy to read in isolation.
    /// Trivial branches (`List`, `Detach`, `ReleaseInput`) stay inline.
    async fn handle(
        self: &Arc<Self>,
        cmd: ClientCommand,
        event_tx: &EventTx,
        subs: &mut SubsMap,
        leases: &mut LeasesMap,
    ) {
        match cmd {
            ClientCommand::Spawn {
                agent,
                project_id,
                model,
                permission,
                resume,
                tags,
            } => {
                let preset: AgentPreset = match agent.parse() {
                    Ok(p) => p,
                    Err(e) => {
                        send_error(event_tx, None, ErrorCode::SpawnFailed, e);
                        return;
                    }
                };
                self.handle_spawn(
                    preset, project_id, model, permission, resume, tags, event_tx,
                )
                .await
            }
            ClientCommand::Resume { session, tags } => {
                self.handle_resume(session, tags, event_tx).await
            }
            ClientCommand::Attach { session, from_seq } => {
                self.handle_attach(session, from_seq, event_tx, subs).await
            }
            ClientCommand::AcquireInput { session } => {
                self.handle_acquire_input(session, event_tx, leases).await
            }
            ClientCommand::Send { session, text } => {
                Self::handle_send(session, text, event_tx, leases)
            }
            ClientCommand::CancelTurn { session } => {
                self.handle_cancel_turn(session, event_tx, leases).await
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
                self.handle_close(session, event_tx, subs, leases).await
            }
            ClientCommand::SetModel { session, model } => {
                self.handle_set_model(session, model, event_tx).await
            }
            ClientCommand::SetTags { session, tags } => {
                self.handle_set_tags(session, tags, event_tx).await
            }
            ClientCommand::List => self.handle_list(event_tx).await,
            ClientCommand::ReadJournal {
                session,
                from_seq,
                max_entries,
            } => {
                self.handle_read_journal(session, from_seq, max_entries, event_tx)
                    .await
            }
            ClientCommand::ListArchived => self.handle_list_archived(event_tx).await,
            ClientCommand::DeleteArchive { session } => {
                self.handle_delete_archive(session, event_tx).await
            }
            ClientCommand::WaitForResult {
                session,
                since_seq,
                timeout_ms,
            } => {
                self.handle_wait_for_result(session, since_seq, timeout_ms, event_tx)
                    .await
            }
            ClientCommand::Fire {
                target,
                prompt,
                tags,
                timeout_ms,
            } => {
                self.handle_fire(target, prompt, tags, timeout_ms, event_tx)
                    .await
            }
            ClientCommand::Inject { .. } => {
                let _ = event_tx.send(ServerEvent::Error {
                    session: None,
                    code: crate::control::ErrorCode::BadRequest,
                    message: "Inject not yet implemented".into(),
                });
            }
            ClientCommand::ListProjects => self.handle_list_projects(event_tx).await,
            ClientCommand::CreateProject { name } => {
                self.handle_create_project(name, event_tx).await
            }
            ClientCommand::DeleteProject { project_id } => {
                self.handle_delete_project(project_id, event_tx).await
            }
            ClientCommand::ListAgents => self.handle_list_agents(event_tx).await,
        }
    }

    async fn handle_delete_archive(self: &Arc<Self>, session: String, event_tx: &EventTx) {
        match self.manager.delete_archive(&session).await {
            Ok(()) => {
                let _ = event_tx.send(ServerEvent::Deleted { session });
            }
            Err(e) => send_error(
                event_tx,
                Some(session),
                ErrorCode::DeleteFailed,
                e.to_string(),
            ),
        }
    }

    /// Resolve `(cwd, fixed_session_id)` for spawn. `project_id = Some(id)` →
    /// look up the project, use its `path`, no fixed session id (engine mints
    /// its own). `None` → mint a new UUID, mkdir `<workspace>/<uuid>/`, return
    /// that as cwd with the same UUID as `fixed_session_id` so the engine
    /// reuses it.
    fn resolve_spawn_cwd(&self, project_id: Option<&str>) -> Result<(PathBuf, Option<String>)> {
        match project_id {
            Some(pid) => {
                let path = self.manager.projects().project_path(pid)?;
                Ok((path, None))
            }
            None => {
                let sid = uuid::Uuid::new_v4().to_string();
                let path = self.manager.projects().allocate_orphan_session_dir(&sid)?;
                Ok((path, Some(sid)))
            }
        }
    }

    async fn handle_spawn(
        self: &Arc<Self>,
        agent: AgentPreset,
        project_id: Option<String>,
        model: Option<String>,
        permission: Option<String>,
        resume: Option<String>,
        tags: BTreeMap<String, String>,
        event_tx: &EventTx,
    ) {
        let _ = event_tx.send(ServerEvent::Spawning {
            agent: agent.to_string(),
            project_id: project_id.clone(),
        });
        let (cwd, fixed_session_id) = match self.resolve_spawn_cwd(project_id.as_deref()) {
            Ok(pair) => pair,
            Err(e) => {
                let code = if project_id.is_some() {
                    ErrorCode::NoProject
                } else {
                    ErrorCode::SpawnFailed
                };
                send_error(event_tx, None, code, e.to_string());
                return;
            }
        };
        let cfg = SessionSpawnConfig {
            agent,
            cwd,
            project_id: project_id.clone(),
            model,
            permission,
            resume_cursor: resume,
            fixed_session_id,
            tags,
        };
        match self.manager.spawn(cfg, 256, 1024).await {
            Ok(engine) => {
                let _ = event_tx.send(ServerEvent::Spawned {
                    session: engine.id().to_string(),
                    project_id: engine.project_id().map(str::to_string),
                    resume_cursor: engine.resume_cursor(),
                });
            }
            Err(e) => send_error(event_tx, None, ErrorCode::SpawnFailed, e.to_string()),
        }
    }

    async fn handle_resume(
        self: &Arc<Self>,
        session: String,
        tags: Option<BTreeMap<String, String>>,
        event_tx: &EventTx,
    ) {
        let _ = event_tx.send(ServerEvent::Resuming {
            session: session.clone(),
        });
        match self.manager.resume(&session, 256, 1024).await {
            Ok(engine) => {
                if let Some(new_tags) = tags {
                    let mut merged = engine.tags();
                    merged.extend(new_tags);
                    if let Err(e) = engine.set_tags(merged).await {
                        tracing::warn!(%session, error = %e, "failed to update tags on resume");
                    }
                }
                let _ = event_tx.send(ServerEvent::Resumed {
                    session: engine.id().to_string(),
                    resume_cursor: engine.resume_cursor(),
                });
            }
            Err(e) => send_error(
                event_tx,
                Some(session),
                ErrorCode::ResumeFailed,
                e.to_string(),
            ),
        }
    }

    async fn handle_set_tags(
        self: &Arc<Self>,
        session: String,
        tags: BTreeMap<String, String>,
        event_tx: &EventTx,
    ) {
        let Some(engine) = self.manager.get(&session).await else {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoSession,
                "no live session for that id",
            );
            return;
        };
        match engine.set_tags(tags.clone()).await {
            Ok(()) => {
                let _ = event_tx.send(ServerEvent::SessionUpdated {
                    session,
                    model: None,
                    tags: Some(tags),
                });
            }
            Err(e) => send_error(
                event_tx,
                None,
                ErrorCode::Other("tag_update_failed".into()),
                e.to_string(),
            ),
        }
    }

    async fn handle_list(self: &Arc<Self>, event_tx: &EventTx) {
        let mut sessions = Vec::new();
        for id in self.manager.list().await {
            if let Some(engine) = self.manager.get(&id).await {
                sessions.push(crate::control::SessionInfo {
                    session: id,
                    project_id: engine.project_id().map(str::to_string),
                    agent: engine.agent().to_string(),
                    cwd: engine.cwd().to_string_lossy().to_string(),
                    model: engine.model(),
                    tags: engine.tags(),
                });
            }
        }
        let _ = event_tx.send(ServerEvent::Listed { sessions });
    }

    async fn handle_list_archived(self: &Arc<Self>, event_tx: &EventTx) {
        let mut sessions = Vec::new();
        match self.manager.list_archived().await {
            Ok(ids) => {
                for id in ids {
                    if let Ok(meta) =
                        crate::session_meta::read_metadata(self.manager.journal_dir(), &id).await
                    {
                        sessions.push(crate::control::SessionInfo {
                            session: id,
                            project_id: meta.project_id,
                            agent: meta.agent,
                            cwd: meta.cwd.to_string_lossy().to_string(),
                            model: meta.model,
                            tags: meta.tags,
                        }); // project_id is now Option<String>
                    }
                }
                let _ = event_tx.send(ServerEvent::ListedArchived { sessions });
            }
            Err(e) => send_error(
                event_tx,
                None,
                ErrorCode::ListArchivedFailed,
                format!("failed to list archived: {e}"),
            ),
        }
    }

    async fn handle_wait_for_result(
        self: &Arc<Self>,
        session: String,
        since_seq: Option<Seq>,
        timeout_ms: Option<u64>,
        event_tx: &EventTx,
    ) {
        let Some(engine) = self.manager.get(&session).await else {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoSession,
                "session not live",
            );
            return;
        };

        let since = since_seq.unwrap_or(0);
        let timeout_duration = Duration::from_millis(timeout_ms.unwrap_or(600_000));

        match engine.wait_for_result(since, timeout_duration).await {
            Ok(Some((seq, result, assistant_text))) => {
                let _ = event_tx.send(ServerEvent::ResultReady {
                    session,
                    seq,
                    result,
                    assistant_text,
                });
            }
            Ok(None) => {
                let _ = event_tx.send(ServerEvent::WaitTimeout { session });
            }
            Err(e) => send_error(
                event_tx,
                Some(session),
                ErrorCode::ReadJournalFailed,
                e.to_string(),
            ),
        }
    }

    async fn handle_fire(
        self: &Arc<Self>,
        target: FireTarget,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout_ms: Option<u64>,
        event_tx: &EventTx,
    ) {
        // 1. Spawn or Resume
        let engine = match target {
            FireTarget::Spawn { preset, project_id } => {
                let parsed: AgentPreset = match preset.parse() {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::FireError {
                            session: None,
                            code: ErrorCode::SpawnFailed,
                            message: e,
                        });
                        return;
                    }
                };
                let (cwd, fixed_session_id) = match self.resolve_spawn_cwd(project_id.as_deref()) {
                    Ok(pair) => pair,
                    Err(e) => {
                        let code = if project_id.is_some() {
                            ErrorCode::NoProject
                        } else {
                            ErrorCode::SpawnFailed
                        };
                        let _ = event_tx.send(ServerEvent::FireError {
                            session: None,
                            code,
                            message: e.to_string(),
                        });
                        return;
                    }
                };
                let cfg = SessionSpawnConfig {
                    agent: parsed,
                    cwd,
                    project_id,
                    model: None,
                    permission: None,
                    resume_cursor: None,
                    fixed_session_id,
                    tags,
                };
                match self.manager.spawn(cfg, 256, 1024).await {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::FireError {
                            session: None,
                            code: ErrorCode::SpawnFailed,
                            message: format!("spawn failed: {e}"),
                        });
                        return;
                    }
                }
            }
            FireTarget::Resume { session_id } => {
                match self.manager.resume(&session_id, 256, 1024).await {
                    Ok(e) => {
                        if !tags.is_empty() {
                            let mut merged = e.tags();
                            merged.extend(tags);
                            let _ = e.set_tags(merged).await;
                        }
                        e
                    }
                    Err(e) => {
                        let _ = event_tx.send(ServerEvent::FireError {
                            session: Some(session_id),
                            code: ErrorCode::ResumeFailed,
                            message: format!("resume failed: {e}"),
                        });
                        return;
                    }
                }
            }
        };

        let session_id = engine.id().to_string();

        // 2. Acquire Input + Send
        let lease = match engine.try_acquire_input() {
            Some(l) => l,
            None => {
                let _ = event_tx.send(ServerEvent::FireError {
                    session: Some(session_id),
                    code: ErrorCode::NoLease,
                    message: "session busy".to_string(),
                });
                return;
            }
        };

        let since = engine.next_seq().await;

        if let Err(e) = lease.send(prompt) {
            let _ = event_tx.send(ServerEvent::FireError {
                session: Some(session_id.clone()),
                code: ErrorCode::SendFailed,
                message: format!("send failed: {e}"),
            });
            return;
        }
        drop(lease); // Release lease immediately

        // 3. WaitForResult
        let timeout_duration = Duration::from_millis(timeout_ms.unwrap_or(600_000));
        match engine.wait_for_result(since, timeout_duration).await {
            Ok(Some((seq, result, assistant_text))) => {
                let _ = event_tx.send(ServerEvent::FireDone {
                    session: session_id,
                    seq_range: (since, seq),
                    result,
                    assistant_text,
                });
            }
            Ok(None) => {
                let _ = event_tx.send(ServerEvent::FireTimeout {
                    session: session_id,
                    partial_seq_range: (since, since),
                });
            }
            Err(e) => {
                let _ = event_tx.send(ServerEvent::FireError {
                    session: Some(session_id),
                    code: ErrorCode::ReadJournalFailed,
                    message: format!("wait failed: {e}"),
                });
            }
        }
    }

    async fn handle_list_projects(self: &Arc<Self>, event_tx: &EventTx) {
        let projects = self.manager.projects().list();
        let _ = event_tx.send(ServerEvent::ProjectsListed { projects });
    }

    async fn handle_create_project(self: &Arc<Self>, name: String, event_tx: &EventTx) {
        match self.manager.projects().create_project(&name) {
            Ok(project) => {
                let _ = event_tx.send(ServerEvent::ProjectCreated { project });
            }
            Err(RoyError::ProjectExists { name }) => send_error(
                event_tx,
                None,
                ErrorCode::ProjectExists,
                format!("project already exists: {name}"),
            ),
            Err(RoyError::InvalidProjectName { name, reason }) => send_error(
                event_tx,
                None,
                ErrorCode::InvalidProjectName,
                format!("invalid project name `{name}`: {reason}"),
            ),
            Err(e) => send_error(
                event_tx,
                None,
                ErrorCode::CreateProjectFailed,
                e.to_string(),
            ),
        }
    }

    async fn handle_delete_project(self: &Arc<Self>, project_id: String, event_tx: &EventTx) {
        let session_ids = match self.manager.projects().remove_entry(&project_id) {
            Ok(ids) => ids,
            Err(e) => {
                send_error(event_tx, None, ErrorCode::NoProject, e.to_string());
                return;
            }
        };
        let close_results = futures_util::future::join_all(session_ids.iter().map(|sid| {
            let manager = Arc::clone(&self.manager);
            let sid = sid.clone();
            async move { manager.close(&sid).await }
        }))
        .await;
        for (sid, res) in session_ids.iter().zip(close_results) {
            if let Err(e) = res {
                tracing::warn!(session = %sid, error = %e, "cascade close failed");
            }
        }
        let delete_results = futures_util::future::join_all(session_ids.iter().map(|sid| {
            let manager = Arc::clone(&self.manager);
            let sid = sid.clone();
            async move { manager.delete_archive(&sid).await }
        }))
        .await;
        for (sid, res) in session_ids.iter().zip(delete_results) {
            if let Err(e) = res {
                tracing::warn!(session = %sid, error = %e, "cascade delete failed");
            }
        }
        let deleted = session_ids;
        let _ = event_tx.send(ServerEvent::ProjectDeleted {
            project_id,
            deleted_sessions: deleted,
        });
    }

    async fn handle_list_agents(self: &Arc<Self>, event_tx: &EventTx) {
        use crate::agents_config::{
            config_path, load_or_bootstrap, AgentsConfigError, AgentsConfigStatus, LoadOutcome,
        };

        let path = match config_path() {
            Ok(p) => p,
            Err(e) => {
                send_error(
                    event_tx,
                    None,
                    ErrorCode::ConfigError,
                    &format!("resolve config path: {e}"),
                );
                return;
            }
        };

        let (agents, status) = match load_or_bootstrap(&path).await {
            Ok(LoadOutcome::Ok(cfg)) => (cfg.into_wire(), AgentsConfigStatus::Ok),
            Ok(LoadOutcome::Created) => (vec![], AgentsConfigStatus::Created),
            Err(AgentsConfigError::Parse(e)) => (
                vec![],
                AgentsConfigStatus::Invalid {
                    reason: format!("toml parse error: {e}"),
                },
            ),
            Err(AgentsConfigError::Validate(s)) => {
                (vec![], AgentsConfigStatus::Invalid { reason: s })
            }
            Err(AgentsConfigError::Io(e)) => {
                send_error(
                    event_tx,
                    None,
                    ErrorCode::ConfigError,
                    &format!("config io error at {}: {e}", path.display()),
                );
                return;
            }
        };

        let _ = event_tx.send(ServerEvent::AgentsList {
            agents,
            config_path: path,
            status,
        });
    }

    /// Live engine → subscribe to its broadcast; otherwise fall back to a
    /// read-only archive replay so closed sessions remain inspectable.
    async fn handle_attach(
        self: &Arc<Self>,
        session: String,
        from_seq: Option<crate::journal::Seq>,
        event_tx: &EventTx,
        subs: &mut SubsMap,
    ) {
        if let Some(engine) = self.manager.get(&session).await {
            attach_live(engine, session, from_seq, event_tx, subs).await;
        } else {
            self.attach_archive(session, from_seq, event_tx, subs).await;
        }
    }

    async fn attach_archive(
        self: &Arc<Self>,
        session: String,
        from_seq: Option<crate::journal::Seq>,
        event_tx: &EventTx,
        subs: &mut SubsMap,
    ) {
        let archive = match self.manager.open_archive(&session).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(%session, error = %e, "archive open failed");
                send_error(
                    event_tx,
                    Some(session),
                    ErrorCode::NoSession,
                    format!("no such session (live or archived): {e}"),
                );
                return;
            }
        };
        let entries = match archive.replay_from(from_seq.unwrap_or(0)).await {
            Ok(e) => e,
            Err(e) => {
                send_error(
                    event_tx,
                    Some(session),
                    ErrorCode::ArchiveReadFailed,
                    e.to_string(),
                );
                return;
            }
        };
        let seq_at_attach = entries
            .last()
            .map(|e| e.seq + 1)
            .unwrap_or_else(|| from_seq.unwrap_or(0));
        let (agent, model) =
            crate::session_meta::read_metadata(self.manager.journal_dir(), &session)
                .await
                .map(|m| (m.agent, m.model))
                .unwrap_or_default();
        let _ = event_tx.send(ServerEvent::Attached {
            session: session.clone(),
            seq_at_attach,
            agent,
            model,
        });
        if let Some(prev) = subs.remove(&session) {
            prev.abort();
        }
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
        subs.insert(session, handle);
    }

    async fn handle_acquire_input(
        self: &Arc<Self>,
        session: String,
        event_tx: &EventTx,
        leases: &mut LeasesMap,
    ) {
        let Some(engine) = self.manager.get(&session).await else {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoSession,
                "no such session",
            );
            return;
        };
        let acquired = engine
            .try_acquire_input()
            .map(|lease| {
                leases.insert(session.clone(), lease);
                true
            })
            .unwrap_or(false);
        let _ = event_tx.send(ServerEvent::InputAcquired { session, acquired });
    }

    async fn handle_cancel_turn(
        self: &Arc<Self>,
        session: String,
        event_tx: &EventTx,
        leases: &LeasesMap,
    ) {
        if !leases.contains_key(&session) {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoLease,
                "input lease not held by this connection",
            );
            return;
        }
        let Some(engine) = self.manager.get(&session).await else {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoSession,
                "no such session",
            );
            return;
        };
        if let Err(e) = engine.cancel_turn() {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::CancelFailed,
                e.to_string(),
            );
        }
    }

    fn handle_send(session: String, text: String, event_tx: &EventTx, leases: &LeasesMap) {
        let Some(lease) = leases.get(&session) else {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoLease,
                "input lease not held by this connection",
            );
            return;
        };
        if let Err(e) = lease.send(text) {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::SendFailed,
                e.to_string(),
            );
        }
    }

    async fn handle_set_model(
        self: &Arc<Self>,
        session: String,
        model: String,
        event_tx: &EventTx,
    ) {
        // Only live sessions accept a model swap — archived sessions need
        // a `Resume` first, otherwise the in-memory engine isn't here to
        // hold the new value (the on-disk metadata is what the next
        // resume reads).
        let Some(engine) = self.manager.get(&session).await else {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoSession,
                "no live session for that id",
            );
            return;
        };
        match engine.set_model(model).await {
            Ok(model) => {
                let _ = event_tx.send(ServerEvent::ModelChanged { session, model });
            }
            Err(e) => send_error(
                event_tx,
                Some(session),
                ErrorCode::SetModelFailed,
                e.to_string(),
            ),
        }
    }

    async fn handle_close(
        self: &Arc<Self>,
        session: String,
        event_tx: &EventTx,
        subs: &mut SubsMap,
        leases: &mut LeasesMap,
    ) {
        leases.remove(&session);
        if let Some(h) = subs.remove(&session) {
            h.abort();
        }
        match self.manager.close(&session).await {
            Ok(()) => {
                let _ = event_tx.send(ServerEvent::Closed { session });
            }
            Err(e) => send_error(
                event_tx,
                Some(session),
                ErrorCode::CloseFailed,
                e.to_string(),
            ),
        }
    }

    async fn handle_read_journal(
        self: &Arc<Self>,
        session: String,
        from_seq: Option<crate::journal::Seq>,
        max_entries: Option<usize>,
        event_tx: &EventTx,
    ) {
        let from = from_seq.unwrap_or(0);
        match self.manager.read_journal(&session, from).await {
            Ok(mut entries) => {
                let has_more = if let Some(n) = max_entries {
                    let more = entries.len() > n;
                    entries.truncate(n);
                    more
                } else {
                    false
                };
                let next_seq = entries.last().map(|e| e.seq + 1).unwrap_or(from);
                let _ = event_tx.send(ServerEvent::JournalRead {
                    session,
                    entries,
                    next_seq,
                    has_more,
                });
            }
            Err(e) => send_error(
                event_tx,
                Some(session),
                ErrorCode::ReadJournalFailed,
                e.to_string(),
            ),
        }
    }
}

/// Free function (not a method) — once the live engine is fished out of the
/// manager, the rest of attach is plain channel plumbing that doesn't need
/// access to `self.manager`. Keeps the call site symmetric with `attach_archive`.
async fn attach_live(
    engine: Arc<crate::engine::SessionEngine>,
    session: String,
    from_seq: Option<crate::journal::Seq>,
    event_tx: &EventTx,
    subs: &mut SubsMap,
) {
    let attach = match engine.attach(from_seq).await {
        Ok(a) => a,
        Err(e) => {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::AttachFailed,
                e.to_string(),
            );
            return;
        }
    };
    let agent = engine.agent().to_string();
    let model = engine.model();
    let _ = event_tx.send(ServerEvent::Attached {
        session: session.clone(),
        seq_at_attach: attach.seq_at_attach,
        agent,
        model,
    });
    if let Some(prev) = subs.remove(&session) {
        prev.abort();
    }
    let tx = event_tx.clone();
    let sid = session.clone();
    let mut stream = attach.stream;
    let handle = tokio::spawn(async move {
        while let Some(entry) = stream.next().await {
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
    subs.insert(session, handle);
}

/// Only panics in the writer task are interesting; clean returns and
/// cancellations stay silent.
fn log_writer_join(res: std::result::Result<(), tokio::task::JoinError>) {
    if let Err(e) = res {
        if e.is_panic() {
            tracing::error!(error = %e, "writer task panicked");
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
            _agent: AgentPreset,
            _model: Option<&str>,
            _permission: Option<&str>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(AcpTransport::new(AcpConfig {
                command: "python3".to_string(),
                args: vec!["tests/scripts/fake-acp-agent.py".to_string()],
                mode_id: Some("yolo".to_string()),
                permission_policy: PermissionPolicy::AllowAll,
                open_timeout: Duration::from_secs(5),
                env_remove: Vec::new(),
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

    /// `load_or_create_ws_token` creates the file with `0600` on first call and
    /// returns the same value on subsequent calls.
    #[cfg(unix)]
    #[test]
    fn ws_token_is_persistent_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let token_path = dir.join("daemon.token");
        let t1 = load_or_create_ws_token(&token_path).unwrap();
        assert!(!t1.is_empty(), "token must not be empty");
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be 0600, got {mode:o}");
        let t2 = load_or_create_ws_token(&token_path).unwrap();
        assert_eq!(t1, t2, "second call must return the persisted token");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Client that omits the token (or sends the wrong one) must be rejected at
    /// the WS upgrade — `accept_hdr_async` returns an Err and the stream never
    /// reaches `serve_ws_connection`.
    #[tokio::test]
    async fn ws_handshake_rejects_missing_or_wrong_token() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let token = Arc::new("the-real-token".to_string());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let token_for_server = Arc::clone(&token);
        let server_task = tokio::spawn(async move {
            // Accept three handshakes; only the third (correct token) succeeds.
            let mut results = Vec::new();
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                let cb = ws_auth_callback(Arc::clone(&token_for_server));
                let r = tokio_tungstenite::accept_hdr_async(stream, cb).await;
                results.push(r.is_ok());
            }
            results
        });

        // 1. No token → fail.
        let url = format!("ws://{addr}");
        let r1 = tokio_tungstenite::connect_async(&url).await;
        assert!(r1.is_err(), "handshake without token must be rejected");

        // 2. Wrong token → fail.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_static("wrong-token"),
        );
        let r2 = tokio_tungstenite::connect_async(req).await;
        assert!(r2.is_err(), "handshake with wrong token must be rejected");

        // 3. Correct token → success.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_str(&token).unwrap(),
        );
        let r3 = tokio_tungstenite::connect_async(req).await;
        assert!(r3.is_ok(), "handshake with correct token must succeed");

        let server_results = server_task.await.unwrap();
        assert_eq!(server_results, vec![false, false, true]);
    }

    /// Unix socket and its parent directory must be created with owner-only
    /// permissions — a sibling user on a shared box must NOT be able to
    /// connect to the socket and drive the control protocol.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_and_parent_dir_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir();
        let socket_path = dir.join("daemon.sock");
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );
        let socket_path_for_task = socket_path.clone();
        let listener_handle = {
            let d = Arc::clone(&daemon);
            tokio::spawn(async move { d.run_unix(&socket_path_for_task).await })
        };
        // Wait for run_unix to bind + chmod the socket.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if socket_path.exists() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("daemon did not bind socket within 2s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let socket_mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            socket_mode, 0o600,
            "socket must be 0600, got {socket_mode:o}"
        );
        let parent_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            parent_mode, 0o700,
            "parent dir must be 0700, got {parent_mode:o}"
        );
        listener_handle.abort();
        let _ = listener_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end through the daemon over an in-memory duplex pipe.
    #[tokio::test]
    async fn spawn_attach_send_round_trip_over_duplex() {
        let dir = tmp_dir();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Spawning { agent, project_id } => {
                assert_eq!(agent, "opencode");
                assert_eq!(project_id, None);
            }
            other => panic!("expected Spawning ack, got {other:?}"),
        }
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
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
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
                assert!(
                    sessions.iter().any(|s| s.session == session),
                    "archive list missing closed session"
                );
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
        assert!(
            saw_result,
            "archive replay must include the terminal Result"
        );

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
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
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
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
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
                tags: None,
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Resuming {
                session: resuming_id,
            } => assert_eq!(resuming_id, session, "Resuming must echo the requested id"),
            other => panic!("expected Resuming ack, got {other:?}"),
        }
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
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
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
                project_id: None,
                model: None,
                permission: None,
                resume: Some("prior-session-sid".into()),
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
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
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        match ws_recv(&mut ws).await {
            ServerEvent::Spawning { agent, project_id } => {
                assert_eq!(agent, "opencode");
                assert_eq!(project_id, None);
            }
            other => panic!("expected Spawning ack, got {other:?}"),
        }
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

    #[tokio::test]
    async fn wait_for_result_resolves_when_turn_finishes() {
        let dir = tmp_dir();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

        // Connection 1: for Spawn + Send
        let (client1_side, server1_side) = tokio::io::duplex(8192);
        let (server1_rd, server1_wr) = tokio::io::split(server1_side);
        let _serve1 = {
            let d = Arc::clone(&daemon);
            tokio::spawn(async move {
                let _ = d.serve_connection(server1_rd, server1_wr).await;
            })
        };
        let (client1_rd, mut client1_wr) = tokio::io::split(client1_side);
        let mut events1 = BufReader::new(client1_rd).lines();

        // Connection 2: for WaitForResult (long poll)
        let (client2_side, server2_side) = tokio::io::duplex(8192);
        let (server2_rd, server2_wr) = tokio::io::split(server2_side);
        let _serve2 = {
            let d = Arc::clone(&daemon);
            tokio::spawn(async move {
                let _ = d.serve_connection(server2_rd, server2_wr).await;
            })
        };
        let (client2_rd, mut client2_wr) = tokio::io::split(client2_side);
        let mut events2 = BufReader::new(client2_rd).lines();

        // 1. Spawn on Connection 1.
        send_cmd_line(
            &mut client1_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events1).await; // Spawning ack
        let session = match next_event_line(&mut events1).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };

        // 2. Start WaitForResult on Connection 2 in the background.
        let wait_handle = {
            let session = session.clone();
            tokio::spawn(async move {
                send_cmd_line(
                    &mut client2_wr,
                    &ClientCommand::WaitForResult {
                        session,
                        since_seq: None,
                        timeout_ms: None,
                    },
                )
                .await;
                next_event_line(&mut events2).await
            })
        };

        // 3. Acquire Input + Send on Connection 1 (trigger the turn).
        send_cmd_line(
            &mut client1_wr,
            &ClientCommand::AcquireInput {
                session: session.clone(),
            },
        )
        .await;
        let _ = next_event_line(&mut events1).await; // InputAcquired
        send_cmd_line(
            &mut client1_wr,
            &ClientCommand::Send {
                session: session.clone(),
                text: "wait for me".into(),
            },
        )
        .await;

        // 4. WaitForResult should resolve.
        match wait_handle.await.unwrap() {
            ServerEvent::ResultReady {
                session: res_sid,
                assistant_text,
                ..
            } => {
                assert_eq!(res_sid, session);
                assert!(!assistant_text.is_empty());
            }
            other => panic!("expected ResultReady, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fire_combo_spawns_sends_and_waits() {
        let dir = tmp_dir();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
            &ClientCommand::Fire {
                target: FireTarget::Spawn {
                    preset: "opencode".into(),
                    project_id: None,
                },
                prompt: "fire now".into(),
                tags: BTreeMap::from([(
                    "roy-scheduler:kind".to_string(),
                    "background_fire".to_string(),
                )]),
                timeout_ms: None,
            },
        )
        .await;

        match next_event_line(&mut events).await {
            ServerEvent::FireDone {
                session,
                assistant_text,
                ..
            } => {
                assert!(!session.is_empty());
                assert!(!assistant_text.is_empty());

                // Verify tags were persisted.
                let meta = crate::session_meta::read_metadata(&dir, &session)
                    .await
                    .unwrap();
                assert_eq!(
                    meta.tags.get("roy-scheduler:kind").unwrap(),
                    "background_fire"
                );
            }
            other => panic!("expected FireDone, got {other:?}"),
        }

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn set_tags_replaces_the_tag_map() {
        let dir = tmp_dir();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

        let (client_side, server_side) = tokio::io::duplex(8192);
        let (server_rd, server_wr) = tokio::io::split(server_side);
        let _serve = {
            let d = Arc::clone(&daemon);
            tokio::spawn(async move {
                let _ = d.serve_connection(server_rd, server_wr).await;
            })
        };
        let (client_rd, mut client_wr) = tokio::io::split(client_side);
        let mut events = BufReader::new(client_rd).lines();

        // Spawn with two tags.
        let mut initial = BTreeMap::new();
        initial.insert("a".to_string(), "1".to_string());
        initial.insert("b".to_string(), "2".to_string());
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: initial,
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
        let session = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };

        // SetTags with only key "b" — "a" must disappear (REPLACE, not merge).
        let mut replacement = BTreeMap::new();
        replacement.insert("b".to_string(), "new".to_string());
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::SetTags {
                session: session.clone(),
                tags: replacement.clone(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::SessionUpdated { tags: Some(t), .. } => {
                assert_eq!(t, replacement, "SetTags must replace, not merge");
            }
            other => panic!("expected SessionUpdated, got {other:?}"),
        }

        // Confirm List reports the replaced map too.
        send_cmd_line(&mut client_wr, &ClientCommand::List).await;
        match next_event_line(&mut events).await {
            ServerEvent::Listed { sessions } => {
                let s = sessions.iter().find(|s| s.session == session).unwrap();
                assert_eq!(s.tags, replacement);
            }
            other => panic!("expected Listed, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v2: CreateProject by name → daemon creates <workspace>/<name>/; then
    /// list it, delete it, verify gone.
    #[tokio::test]
    async fn create_list_delete_roundtrip() {
        let dir = tmp_dir();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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

        // CreateProject with a valid name — daemon creates the dir.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::CreateProject {
                name: "proj-alpha".into(),
            },
        )
        .await;
        let project = match next_event_line(&mut events).await {
            ServerEvent::ProjectCreated { project } => project,
            other => panic!("expected ProjectCreated, got {other:?}"),
        };
        assert_eq!(project.name, "proj-alpha");
        assert!(project.path.is_dir(), "daemon must create workspace dir");

        // ListProjects
        send_cmd_line(&mut client_wr, &ClientCommand::ListProjects).await;
        let listed = match next_event_line(&mut events).await {
            ServerEvent::ProjectsListed { projects } => projects,
            other => panic!("expected ProjectsListed, got {other:?}"),
        };
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, project.id);

        // CreateProject with invalid name → InvalidProjectName
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::CreateProject {
                name: "bad/name".into(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Error { code, .. } => {
                assert_eq!(
                    code,
                    ErrorCode::InvalidProjectName,
                    "bad name must yield invalid_project_name"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }

        // DeleteProject — no sessions, so deleted_sessions empty
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::DeleteProject {
                project_id: project.id.clone(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::ProjectDeleted {
                project_id,
                deleted_sessions,
            } => {
                assert_eq!(project_id, project.id);
                assert!(deleted_sessions.is_empty());
            }
            other => panic!("expected ProjectDeleted, got {other:?}"),
        }

        // Verify gone
        send_cmd_line(&mut client_wr, &ClientCommand::ListProjects).await;
        match next_event_line(&mut events).await {
            ServerEvent::ProjectsListed { projects } => assert!(projects.is_empty()),
            other => panic!("expected ProjectsListed, got {other:?}"),
        }

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v2: Create a project explicitly, then spawn a session attached to it.
    /// The session's cwd must be the project's workspace dir.
    #[tokio::test]
    async fn create_project_then_spawn_attaches() {
        let dir = tmp_dir();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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

        // Create project.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::CreateProject {
                name: "myproj".into(),
            },
        )
        .await;
        let project = match next_event_line(&mut events).await {
            ServerEvent::ProjectCreated { project } => project,
            other => panic!("expected ProjectCreated, got {other:?}"),
        };

        // Spawn into that project.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                project_id: Some(project.id.clone()),
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
        let (spawned_pid, session_id) = match next_event_line(&mut events).await {
            ServerEvent::Spawned {
                project_id,
                session,
                ..
            } => (project_id, session),
            other => panic!("expected Spawned, got {other:?}"),
        };
        assert_eq!(spawned_pid.as_deref(), Some(project.id.as_str()));

        // Session must be registered under the project.
        let sids = daemon.manager.projects().sessions_in(&project.id);
        assert!(
            sids.contains(&session_id),
            "session must be in project's member list"
        );

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v2: Spawning without a project_id creates an orphan session in
    /// <workspace>/<session_id>/.
    #[tokio::test]
    async fn spawn_without_project_creates_orphan_dir() {
        let dir = tmp_dir();
        let workspace = dir.join("workspace");
        let daemon = Arc::new(
            Daemon::new(dir.clone(), workspace.clone(), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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
                project_id: None, // orphan
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
        let (project_id, session_id) = match next_event_line(&mut events).await {
            ServerEvent::Spawned {
                project_id,
                session,
                ..
            } => (project_id, session),
            other => panic!("expected Spawned, got {other:?}"),
        };
        assert!(project_id.is_none(), "orphan spawn must have no project_id");

        // The orphan dir must exist at <workspace>/<session_id>/.
        let orphan_dir = workspace.join(&session_id);
        assert!(
            orphan_dir.is_dir(),
            "orphan dir must exist at {}",
            orphan_dir.display()
        );

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v2: cascade-delete removes journal + meta files for sessions in a project.
    #[tokio::test]
    async fn cascade_delete_removes_journal_files() {
        let dir = tmp_dir();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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

        // Create project first.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::CreateProject {
                name: "cascade-proj".into(),
            },
        )
        .await;
        let project = match next_event_line(&mut events).await {
            ServerEvent::ProjectCreated { project } => project,
            other => panic!("expected ProjectCreated, got {other:?}"),
        };

        // Spawn a session into that project.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                project_id: Some(project.id.clone()),
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let _ = next_event_line(&mut events).await; // Spawning ack
        let session_id = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };

        let jsonl = dir.join(format!("{session_id}.jsonl"));
        let meta = dir.join(format!("{session_id}.meta.json"));
        assert!(jsonl.exists(), "journal must exist after spawn");
        assert!(meta.exists(), "meta must exist after spawn");

        send_cmd_line(
            &mut client_wr,
            &ClientCommand::DeleteProject {
                project_id: project.id.clone(),
            },
        )
        .await;
        let deleted = match next_event_line(&mut events).await {
            ServerEvent::ProjectDeleted {
                deleted_sessions, ..
            } => deleted_sessions,
            other => panic!("expected ProjectDeleted, got {other:?}"),
        };
        assert_eq!(deleted, vec![session_id.clone()]);
        assert!(!jsonl.exists(), "journal must be erased by cascade");
        assert!(!meta.exists(), "meta must be erased by cascade");

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
    }

    /// Spin up a fresh Daemon, send one command, read one ServerEvent, return it.
    async fn run_command_against_daemon(cmd: ClientCommand) -> ServerEvent {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let daemon = Arc::new(
            Daemon::new(dir.clone(), dir.join("workspace"), Arc::new(FakeAcpFactory))
                .expect("registry load"),
        );

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

        send_cmd_line(&mut client_wr, &cmd).await;
        let ev = next_event_line(&mut events).await;

        drop(client_wr);
        drop(events);
        let _ = serve_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
        ev
    }

    // ── ListAgents integration tests ────────────────────────────────────────

    use crate::agents_config::AgentsConfigStatus;

    #[tokio::test]
    async fn list_agents_returns_ok_for_valid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("agents.toml");
        tokio::fs::write(
            &cfg_path,
            r#"
            [[agent]]
            preset = "claude"
            [[agent.models]]
            id = "claude-sonnet-4-6"
            default = true
        "#,
        )
        .await
        .unwrap();

        temp_env::async_with_vars(
            [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
            async {
                let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
                let ServerEvent::AgentsList { agents, status, .. } = ev else {
                    panic!("got {ev:?}");
                };
                assert!(matches!(status, AgentsConfigStatus::Ok));
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].preset, crate::agents_config::AgentPreset::Claude);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn list_agents_bootstraps_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("missing.toml");
        temp_env::async_with_vars(
            [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
            async {
                let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
                let ServerEvent::AgentsList { agents, status, .. } = ev else {
                    panic!()
                };
                assert!(matches!(status, AgentsConfigStatus::Created));
                assert!(agents.is_empty());
                assert!(cfg_path.exists());
            },
        )
        .await;
    }

    #[tokio::test]
    async fn list_agents_reports_invalid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("agents.toml");
        tokio::fs::write(&cfg_path, "this is not toml [[[")
            .await
            .unwrap();
        temp_env::async_with_vars(
            [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
            async {
                let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
                let ServerEvent::AgentsList { status, agents, .. } = ev else {
                    panic!()
                };
                assert!(agents.is_empty());
                assert!(matches!(status, AgentsConfigStatus::Invalid { .. }));
            },
        )
        .await;
    }

    #[tokio::test]
    async fn list_agents_reports_validation_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("agents.toml");
        tokio::fs::write(
            &cfg_path,
            r#"
            [[agent]]
            preset = "claude"
            [[agent]]
            preset = "claude"
        "#,
        )
        .await
        .unwrap();
        temp_env::async_with_vars(
            [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
            async {
                let ev = run_command_against_daemon(ClientCommand::ListAgents).await;
                let ServerEvent::AgentsList { status, .. } = ev else {
                    panic!()
                };
                let AgentsConfigStatus::Invalid { reason } = status else {
                    panic!()
                };
                assert!(reason.contains("duplicate"), "got: {reason}");
            },
        )
        .await;
    }

    #[tokio::test]
    async fn list_agents_concurrent_bootstrap_is_safe() {
        // Two tasks race on a clean config path. Atomic rename means the
        // "loser" silently overwrites with identical sample content. Both
        // must return Created, neither may panic, the file must end up
        // readable and equal to SAMPLE_TOML.
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("missing.toml");
        temp_env::async_with_vars(
            [("ROY_AGENTS_CONFIG", Some(cfg_path.to_str().unwrap()))],
            async {
                let (a, b) = tokio::join!(
                    run_command_against_daemon(ClientCommand::ListAgents),
                    run_command_against_daemon(ClientCommand::ListAgents),
                );
                for ev in [a, b] {
                    let ServerEvent::AgentsList { status, .. } = ev else {
                        panic!("expected AgentsList, got {ev:?}")
                    };
                    assert!(
                        matches!(status, AgentsConfigStatus::Created),
                        "expected Created, got {status:?}"
                    );
                }
                let written = tokio::fs::read_to_string(&cfg_path).await.unwrap();
                assert_eq!(written, crate::agents_config::SAMPLE_TOML);
            },
        )
        .await;
    }
}
