//! ACP transport built on the official `agent-client-protocol` SDK.
//!
//! Why we spawn the child ourselves instead of using `AcpAgent::from_args`:
//! `AcpAgent`'s built-in child monitor treats a clean `exit(0)` as normal
//! shutdown and returns `Ok(())`, after which the SDK's `run_until` keeps
//! awaiting our foreground task forever — a pending `send_request` never
//! resolves because the JSON-RPC dispatch loop has already finished. The
//! agent dying mid-turn is then indistinguishable from "still working".
//!
//! Instead we own the `Child` directly, hand the SDK the child's stdio as
//! `ByteStreams`, and run a `child.wait()` watcher in the same task. When
//! the process exits — for any reason — the watcher drops a `watch::Sender`
//! and `run_session` / `run_turn` bail out via `dead_rx.changed()`,
//! producing a terminal `Result { stop_reason: Error }`.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use agent_client_protocol::schema::{
    CancelNotification, ContentBlock, ContentChunk, InitializeRequest, LoadSessionRequest,
    NewSessionRequest, PermissionOptionKind, PromptRequest, ProtocolVersion,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate,
    SetSessionModeRequest, StopReason as AcpStopReason, TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};

use crate::error::{Result, RoyError};
use crate::event::{StopReason, TurnEvent};

use super::{Handle, Transport, TurnStream};

/// Shared sink that the global notification handler writes into. `Some(tx)`
/// while a turn is active, `None` otherwise (updates outside a turn — e.g. a
/// `session/load` history replay — are dropped).
type TurnSink = Arc<Mutex<Option<mpsc::UnboundedSender<TurnEvent>>>>;

/// How to answer agent-initiated `session/request_permission` requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionPolicy {
    /// Reject every permission request.
    Deny,
    /// Approve every permission request by selecting an allow option.
    AllowAll,
}

/// Launch + behaviour config for an ACP agent.
pub struct AcpConfig {
    pub command: String,
    pub args: Vec<String>,
    /// ACP mode to set after the session opens (e.g. "yolo" to auto-approve).
    pub mode_id: Option<String>,
    pub permission_policy: PermissionPolicy,
    /// Upper bound on the open handshake (spawn + initialize + session). Guards
    /// against an agent that accepts the connection but never replies.
    pub open_timeout: Duration,
}

impl AcpConfig {
    /// gemini --acp --skip-trust, auto-approving tools via yolo mode.
    pub fn gemini() -> Self {
        Self {
            command: "gemini".to_string(),
            args: vec!["--acp".to_string(), "--skip-trust".to_string()],
            mode_id: Some("yolo".to_string()),
            permission_policy: PermissionPolicy::AllowAll,
            open_timeout: Duration::from_secs(30),
        }
    }

    /// opencode acp. OpenCode has no ACP "modes" (it exposes configOptions
    /// instead), so no set_mode is sent.
    pub fn opencode() -> Self {
        Self {
            command: "opencode".to_string(),
            args: vec!["acp".to_string()],
            mode_id: None,
            permission_policy: PermissionPolicy::Deny,
            open_timeout: Duration::from_secs(30),
        }
    }

    /// codex via the codex-acp adapter, using its `full-access` mode.
    pub fn codex() -> Self {
        Self {
            command: "codex-acp".to_string(),
            args: Vec::new(),
            mode_id: Some("full-access".to_string()),
            permission_policy: PermissionPolicy::AllowAll,
            open_timeout: Duration::from_secs(30),
        }
    }

    /// Claude Code via the claude-code-acp adapter. No ACP modes; auto-approve
    /// tools.
    pub fn claude_agent() -> Self {
        Self {
            command: "claude-code-acp".to_string(),
            args: Vec::new(),
            mode_id: None,
            permission_policy: PermissionPolicy::AllowAll,
            open_timeout: Duration::from_secs(30),
        }
    }
}

pub struct AcpTransport {
    config: AcpConfig,
}

impl AcpTransport {
    pub fn new(config: AcpConfig) -> Self {
        Self { config }
    }
}

/// One unit of work for the session actor.
enum SessionCommand {
    Prompt {
        text: String,
        event_tx: mpsc::UnboundedSender<TurnEvent>,
        /// Resolves (value or sender-drop) when the caller abandons the turn,
        /// signalling the actor to send `session/cancel`.
        cancel_rx: oneshot::Receiver<()>,
    },
    Close,
}

#[async_trait]
impl Transport for AcpTransport {
    async fn open(
        &self,
        _session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>> {
        let cwd = std::path::absolute(&cwd).map_err(RoyError::Io)?;

        let mut child = Command::new(&self.config.command)
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(RoyError::Io)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RoyError::Protocol("child stdin not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RoyError::Protocol("child stdout not piped".into()))?;
        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

        let policy = self.config.permission_policy;
        let mode_id = self.config.mode_id.clone();
        let resume = resume_cursor.map(str::to_string);
        let open_timeout = self.config.open_timeout;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
        let (ready_tx, ready_rx) = oneshot::channel::<String>();

        let sink: TurnSink = Arc::new(Mutex::new(None));
        let sink_for_notif = Arc::clone(&sink);
        let sink_for_actor = Arc::clone(&sink);

        // Watch channel: sender lives in the watcher branch and is dropped
        // when the child exits, signalling every cloned receiver via `changed()`.
        let (dead_tx, dead_rx) = watch::channel(());

        // Inline child-monitor + session driver. Keeping them in the same task
        // means cancelling the task (e.g. on `open_timeout`) drops `child` and
        // `kill_on_drop` fires — no orphan watcher task.
        let task = tokio::spawn(async move {
            let watcher = async move {
                let _ = child.wait().await;
                drop(dead_tx);
                // Keep child owned so it isn't dropped (and killed) while the
                // session future might still be processing buffered messages.
                std::future::pending::<()>().await;
            };

            let session = Client
                .builder()
                .name("roy")
                .on_receive_notification(
                    async move |notif: SessionNotification, _cx| {
                        if let Some(event) = update_to_event(notif.update) {
                            if let Some(tx) = sink_for_notif.lock().unwrap().as_ref() {
                                let _ = tx.send(event);
                            }
                        }
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |request: RequestPermissionRequest, responder, _cx| {
                        let outcome = permission_outcome(&request, policy);
                        responder.respond(RequestPermissionResponse::new(outcome))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
                    run_session(
                        cx,
                        cwd,
                        resume,
                        mode_id,
                        ready_tx,
                        cmd_rx,
                        sink_for_actor,
                        dead_rx,
                    )
                    .await
                });

            tokio::select! {
                r = session => r,
                _ = watcher => unreachable!("watcher branch is pending::<()>"),
            }
        });

        match tokio::time::timeout(open_timeout, ready_rx).await {
            Ok(Ok(acp_sid)) => Ok(Box::new(AcpHandle { cmd_tx, acp_sid })),
            Ok(Err(_)) => match task.await {
                Ok(Err(err)) => Err(RoyError::Protocol(err.to_string())),
                _ => Err(RoyError::ProcessExited),
            },
            Err(_) => {
                task.abort();
                Err(RoyError::Timeout(open_timeout))
            }
        }
    }
}

/// Drive one connection: handshake, open the session, then serve commands until
/// the handle closes or the child process dies.
async fn run_session(
    cx: ConnectionTo<Agent>,
    cwd: PathBuf,
    resume: Option<String>,
    mode_id: Option<String>,
    ready_tx: oneshot::Sender<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    sink: TurnSink,
    mut dead_rx: watch::Receiver<()>,
) -> std::result::Result<(), agent_client_protocol::Error> {
    let session_id = tokio::select! {
        r = setup_session(&cx, cwd, resume, mode_id) => r?,
        _ = dead_rx.changed() => {
            return Err(agent_client_protocol::Error::internal_error()
                .data("agent process exited during initialize"));
        }
    };

    if ready_tx.send(session_id.to_string()).is_err() {
        // Caller stopped waiting on `open`; nothing to serve.
        return Ok(());
    }

    loop {
        let cmd = tokio::select! {
            biased;
            _ = dead_rx.changed() => return Ok(()),
            c = cmd_rx.recv() => match c {
                Some(c) => c,
                None => return Ok(()),
            },
        };
        match cmd {
            SessionCommand::Prompt {
                text,
                event_tx,
                cancel_rx,
            } => {
                run_turn(
                    &cx,
                    &session_id,
                    &text,
                    &event_tx,
                    cancel_rx,
                    &sink,
                    &mut dead_rx,
                )
                .await?
            }
            SessionCommand::Close => break,
        }
    }
    Ok(())
}

async fn setup_session(
    cx: &ConnectionTo<Agent>,
    cwd: PathBuf,
    resume: Option<String>,
    mode_id: Option<String>,
) -> std::result::Result<SessionId, agent_client_protocol::Error> {
    cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;

    let (session_id, modes) = match resume {
        Some(sid) => {
            cx.send_request(LoadSessionRequest::new(sid.clone(), cwd))
                .block_task()
                .await?;
            (SessionId::from(sid), None)
        }
        None => {
            let response = cx
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;
            (response.session_id, response.modes)
        }
    };

    if let Some(mode) = mode_id {
        if let Some(state) = &modes {
            let available = state
                .available_modes
                .iter()
                .any(|m| m.id.0.as_ref() == mode);
            if !available {
                return Err(agent_client_protocol::Error::internal_error()
                    .data(format!("mode '{mode}' is not available for this session")));
            }
        }
        cx.send_request(SetSessionModeRequest::new(session_id.clone(), mode))
            .block_task()
            .await?;
    }

    Ok(session_id)
}

/// Stream one prompt turn into `event_tx` until the prompt response resolves.
/// If the caller drops the stream early, `cancel_rx` resolves and we send
/// `session/cancel`, then drain the (cancelled) response so we emit a terminal
/// `Result`. If the child dies mid-turn, `dead_rx` fires and we synthesize a
/// terminal `Result { Error }` instead — the SDK would never resolve the
/// pending request on its own.
async fn run_turn(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    text: &str,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    cancel_rx: oneshot::Receiver<()>,
    sink: &Mutex<Option<mpsc::UnboundedSender<TurnEvent>>>,
    dead_rx: &mut watch::Receiver<()>,
) -> std::result::Result<(), agent_client_protocol::Error> {
    // Install sink so the global notification handler forwards updates here.
    *sink.lock().unwrap() = Some(event_tx.clone());

    let prompt = PromptRequest::new(
        session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(text.to_string()))],
    );
    let mut prompt_fut = Box::pin(cx.send_request(prompt).block_task());

    let stop_reason = tokio::select! {
        r = &mut prompt_fut => match r {
            Ok(resp) => map_stop_reason(resp.stop_reason),
            Err(_) => StopReason::Error,
        },
        _ = cancel_rx => {
            let _ = cx.send_notification(CancelNotification::new(session_id.clone()));
            match prompt_fut.await {
                Ok(resp) => map_stop_reason(resp.stop_reason),
                Err(_) => StopReason::Error,
            }
        }
        _ = dead_rx.changed() => StopReason::Error,
    };

    let _ = event_tx.send(TurnEvent::Result {
        cost_usd: None,
        stop_reason,
    });

    *sink.lock().unwrap() = None;
    Ok(())
}

fn update_to_event(update: SessionUpdate) -> Option<TurnEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::Text(text),
            ..
        }) => Some(TurnEvent::AssistantText { text: text.text }),
        SessionUpdate::ToolCall(tool_call) => Some(TurnEvent::ToolUse {
            name: tool_call.title,
            input: tool_call.raw_input.unwrap_or(Value::Null),
        }),
        SessionUpdate::AvailableCommandsUpdate(_) => None,
        other => serde_json::to_value(&other).ok().map(TurnEvent::Raw),
    }
}

fn map_stop_reason(stop: AcpStopReason) -> StopReason {
    match stop {
        AcpStopReason::EndTurn => StopReason::EndTurn,
        AcpStopReason::MaxTokens => StopReason::MaxTokens,
        AcpStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        AcpStopReason::Refusal => StopReason::Refusal,
        AcpStopReason::Cancelled => StopReason::Cancelled,
        _ => StopReason::Error,
    }
}

fn permission_outcome(
    request: &RequestPermissionRequest,
    policy: PermissionPolicy,
) -> RequestPermissionOutcome {
    if policy == PermissionPolicy::Deny {
        return RequestPermissionOutcome::Cancelled;
    }
    let chosen = request
        .options
        .iter()
        .find(|o| o.kind == PermissionOptionKind::AllowOnce)
        .or_else(|| {
            request
                .options
                .iter()
                .find(|o| o.kind == PermissionOptionKind::AllowAlways)
        })
        .or_else(|| request.options.first());

    match chosen {
        Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            option.option_id.clone(),
        )),
        None => RequestPermissionOutcome::Cancelled,
    }
}

pub struct AcpHandle {
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    acp_sid: String,
}

#[async_trait]
impl Handle for AcpHandle {
    async fn send(&mut self, prompt: &str) -> Result<TurnStream<'_>> {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        self.cmd_tx
            .send(SessionCommand::Prompt {
                text: prompt.to_string(),
                event_tx,
                cancel_rx,
            })
            .map_err(|_| RoyError::ProcessExited)?;
        Ok(turn_stream(event_rx, cancel_tx))
    }

    fn resume_cursor(&self) -> Option<String> {
        Some(self.acp_sid.clone())
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.cmd_tx.send(SessionCommand::Close);
        Ok(())
    }
}

/// Stream the turn's events. `cancel_tx` is held for the stream's lifetime: if
/// the consumer drops the stream before the terminal `Result`, `cancel_tx` drops
/// too and the actor cancels the turn. On normal completion the actor has
/// already left the turn loop, so the drop is a no-op.
fn turn_stream(
    mut rx: mpsc::UnboundedReceiver<TurnEvent>,
    cancel_tx: oneshot::Sender<()>,
) -> TurnStream<'static> {
    Box::pin(async_stream::stream! {
        let _cancel_tx = cancel_tx;
        let mut saw_result = false;
        while let Some(event) = rx.recv().await {
            let end = matches!(event, TurnEvent::Result { .. });
            saw_result |= end;
            yield event;
            if end {
                break;
            }
        }
        // The actor died without emitting a terminal Result (process exit, task
        // drop). The stream contract still guarantees one.
        if !saw_result {
            yield TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::Error,
            };
        }
    })
}
