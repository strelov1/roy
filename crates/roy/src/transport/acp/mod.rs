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
    CancelNotification, ContentBlock, ContentChunk, InitializeRequest, LoadSessionRequest, Meta,
    NewSessionRequest, PermissionOptionKind, PromptRequest, ProtocolVersion,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate,
    SetSessionModeRequest, StopReason as AcpStopReason, TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};

use crate::error::{Result, RoyError};
use crate::event::{StopReason, TurnEvent};

use super::{CancelSignal, Handle, Transport, TurnStream};

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

/// How a preset accepts a system/persona prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPromptChannel {
    /// Sent via ACP `_meta.systemPrompt = { append }` on `session/new` and
    /// `session/load`. A real system prompt, outside history, survives resume.
    Meta,
    /// The preset ignores `_meta`; the engine injects the persona as the first
    /// journaled turn instead.
    FirstTurn,
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
    /// Env vars to strip from the child's environment at spawn (the daemon's
    /// own env is inherited otherwise). Per-preset because the problematic
    /// variables are agent-specific — e.g. `CLAUDECODE` for `claude-code-acp`.
    pub env_remove: Vec<String>,
    /// Which channel carries the persona prompt for this preset.
    pub system_prompt_channel: SystemPromptChannel,
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
            env_remove: Vec::new(),
            system_prompt_channel: SystemPromptChannel::FirstTurn,
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
            env_remove: Vec::new(),
            system_prompt_channel: SystemPromptChannel::Meta,
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
            env_remove: Vec::new(),
            system_prompt_channel: SystemPromptChannel::FirstTurn,
        }
    }

    /// Claude Code via the claude-code-acp adapter. No ACP modes; auto-approve
    /// tools. `CLAUDECODE` is stripped: claude-code-acp has an anti-recursion
    /// guard that refuses to start if it sees that variable set, which makes
    /// `roy serve` impossible to run from inside another Claude Code session.
    pub fn claude() -> Self {
        Self {
            command: "claude-code-acp".to_string(),
            args: Vec::new(),
            mode_id: None,
            permission_policy: PermissionPolicy::AllowAll,
            open_timeout: Duration::from_secs(30),
            env_remove: vec!["CLAUDECODE".to_string()],
            system_prompt_channel: SystemPromptChannel::Meta,
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
        system_prompt: Option<&str>,
    ) -> Result<Box<dyn Handle>> {
        let cwd = std::path::absolute(&cwd).map_err(RoyError::Io)?;

        // Route the persona to exactly one channel: Meta presets carry it in
        // the session request's `_meta`; FirstTurn presets defer it to the
        // engine (but never on resume — the agent reloads it from history).
        let system_prompt = system_prompt.map(str::to_string);
        let (meta_prompt, pending_persona) = match self.config.system_prompt_channel {
            SystemPromptChannel::Meta => (system_prompt, None),
            SystemPromptChannel::FirstTurn if resume_cursor.is_none() => (None, system_prompt),
            SystemPromptChannel::FirstTurn => (None, None),
        };

        let mut cmd = Command::new(&self.config.command);
        cmd.args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for key in &self.config.env_remove {
            cmd.env_remove(key);
        }
        let mut child = cmd.spawn().map_err(RoyError::Io)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RoyError::Protocol("child stdin not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RoyError::Protocol("child stdout not piped".into()))?;

        let sink: TurnSink = Arc::new(Mutex::new(None));
        let sink_for_filter = Arc::clone(&sink);
        let sink_for_notif = Arc::clone(&sink);
        let sink_for_actor = Arc::clone(&sink);

        // Line-based stdin filter between the child and the SDK:
        //   - intercept `session/update {usage_update}` notifications, extract
        //     tokens/cost into a `TurnEvent::Usage`, and drop the line so the
        //     SDK never sees an unknown variant. This fixes opencode (whose
        //     usage_update was triggering JSON-RPC error replies on what is
        //     supposed to be a notification, aborting the turn).
        //   - everything else passes through unchanged.
        // The duplex's writer half lives in the filter task; when the child
        // exits, BufReader returns None, the writer drops, and the SDK reading
        // the reader half sees EOF.
        let (filter_writer, filter_reader) = tokio::io::duplex(64 * 1024);
        tokio::spawn(filter_child_stdout(stdout, filter_writer, sink_for_filter));
        let transport = ByteStreams::new(stdin.compat_write(), filter_reader.compat());

        let policy = self.config.permission_policy;
        let mode_id = self.config.mode_id.clone();
        let resume = resume_cursor.map(str::to_string);
        let open_timeout = self.config.open_timeout;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
        let (ready_tx, ready_rx) = oneshot::channel::<String>();

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
                        meta_prompt,
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
            Ok(Ok(acp_sid)) => Ok(Box::new(AcpHandle {
                cmd_tx,
                acp_sid,
                pending_persona,
            })),
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
    meta_prompt: Option<String>,
    ready_tx: oneshot::Sender<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    sink: TurnSink,
    mut dead_rx: watch::Receiver<()>,
) -> std::result::Result<(), agent_client_protocol::Error> {
    let session_id = tokio::select! {
        r = setup_session(&cx, cwd, resume, mode_id, meta_prompt) => r?,
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

/// Set `_meta.systemPrompt = { "append": <prompt> }` on a request's meta map.
/// No-op when `prompt` is `None`. claude-code-acp appends this to its
/// `claude_code` preset; honored on both `session/new` and `session/load`.
fn apply_system_prompt_meta(meta: &mut Option<Meta>, prompt: Option<&str>) {
    let Some(prompt) = prompt else { return };
    let map = meta.get_or_insert_with(serde_json::Map::new);
    map.insert(
        "systemPrompt".to_string(),
        serde_json::json!({ "append": prompt }),
    );
}

async fn setup_session(
    cx: &ConnectionTo<Agent>,
    cwd: PathBuf,
    resume: Option<String>,
    mode_id: Option<String>,
    meta_prompt: Option<String>,
) -> std::result::Result<SessionId, agent_client_protocol::Error> {
    cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;

    let (session_id, modes) = match resume {
        Some(sid) => {
            let mut req = LoadSessionRequest::new(sid.clone(), cwd);
            apply_system_prompt_meta(&mut req.meta, meta_prompt.as_deref());
            cx.send_request(req).block_task().await?;
            (SessionId::from(sid), None)
        }
        None => {
            let mut req = NewSessionRequest::new(cwd);
            apply_system_prompt_meta(&mut req.meta, meta_prompt.as_deref());
            let response = cx.send_request(req).block_task().await?;
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

    // Helper: pipe the JSON-RPC error message into the journal as a System
    // event so the UI / late attach can surface what actually broke (e.g.
    // "Authentication required") instead of just an opaque is_error=true.
    let emit_agent_error = |e: &dyn std::fmt::Display| {
        let _ = event_tx.send(TurnEvent::System {
            subtype: format!("agent_error: {}", e),
        });
    };
    let stop_reason = tokio::select! {
        r = &mut prompt_fut => match r {
            Ok(resp) => {
                if let Some(usage) = extract_usage_from_response(&resp) {
                    let _ = event_tx.send(usage);
                }
                map_stop_reason(resp.stop_reason)
            }
            Err(e) => { emit_agent_error(&e); StopReason::Error }
        },
        _ = cancel_rx => {
            let _ = cx.send_notification(CancelNotification::new(session_id.clone()));
            match prompt_fut.await {
                Ok(resp) => {
                    if let Some(usage) = extract_usage_from_response(&resp) {
                        let _ = event_tx.send(usage);
                    }
                    map_stop_reason(resp.stop_reason)
                }
                Err(e) => { emit_agent_error(&e); StopReason::Error }
            }
        }
        _ = dead_rx.changed() => {
            emit_agent_error(&"agent process exited mid-turn");
            StopReason::Error
        }
    };

    // Close the notification gate BEFORE emitting the terminal Result so a
    // late `session/update` (delivered between `prompt_fut` resolving and us
    // returning) can't slip into `event_tx` after `Result` — `turn_stream`
    // breaks on the first `Result`, so anything queued after it is dropped.
    *sink.lock().unwrap() = None;
    let _ = event_tx.send(TurnEvent::Result {
        cost_usd: None,
        stop_reason,
    });
    Ok(())
}

/// Forward lines from the child's stdout to the SDK, intercepting `session/
/// update` notifications whose variant the SDK doesn't model. Currently
/// handles opencode's `usage_update`: extracts tokens/cost into a
/// `TurnEvent::Usage` pushed straight into the active turn's sink, and drops
/// the line so the SDK doesn't reply with a JSON-RPC error on a notification
/// (which would otherwise make opencode abort the turn).
async fn filter_child_stdout(
    stdout: tokio::process::ChildStdout,
    mut sink_out: tokio::io::DuplexStream,
    turn_sink: TurnSink,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(usage) = try_extract_usage_update(&line) {
            if let Some(tx) = turn_sink.lock().unwrap().as_ref() {
                let _ = tx.send(usage);
            }
            continue;
        }
        if sink_out.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if sink_out.write_all(b"\n").await.is_err() {
            break;
        }
    }
}

/// Look for token / cost info on the agent's final `session/prompt` response.
/// `PromptResponse` exposes `stop_reason` typed but `usage` only via
/// passthrough fields — `serde_json::to_value` is the cheapest way to peek
/// across SDK versions without hardcoding their internal shape. Claude and
/// codex usually populate this; others (gemini, opencode) may not, in which
/// case we get `None` and stay silent.
fn extract_usage_from_response<T: serde::Serialize>(resp: &T) -> Option<TurnEvent> {
    let v = serde_json::to_value(resp).ok()?;
    // Tolerate both snake_case and camelCase across agents.
    let usage = v.get("usage").or_else(|| v.get("_meta"))?;
    let pick = |k1: &str, k2: &str| {
        usage
            .get(k1)
            .or_else(|| usage.get(k2))
            .and_then(Value::as_u64)
    };
    let input = pick("input_tokens", "inputTokens");
    let output = pick("output_tokens", "outputTokens");
    let cost = usage
        .get("cost_usd")
        .or_else(|| usage.get("costUsd"))
        .and_then(Value::as_f64);
    if input.is_none() && output.is_none() && cost.is_none() {
        return None;
    }
    Some(TurnEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cost_usd: cost,
    })
}

/// Recognise opencode's `session/update {sessionUpdate: "usage_update"}`
/// notification and pull out the numbers we care about. Returns `None` for
/// any other line — the caller passes those through unchanged.
fn try_extract_usage_update(line: &str) -> Option<TurnEvent> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("method")?.as_str()? != "session/update" {
        return None;
    }
    let update = v.pointer("/params/update")?;
    if update.get("sessionUpdate")?.as_str()? != "usage_update" {
        return None;
    }
    let used = update.get("used").and_then(Value::as_u64);
    let cost = update.pointer("/cost/amount").and_then(Value::as_f64);
    if used.is_none() && cost.is_none() {
        return None;
    }
    Some(TurnEvent::Usage {
        input_tokens: None,
        output_tokens: used,
        cost_usd: cost.filter(|c| *c > 0.0),
    })
}

fn update_to_event(update: SessionUpdate) -> Option<TurnEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::Text(text),
            ..
        }) => Some(TurnEvent::AssistantText { text: text.text }),
        SessionUpdate::AgentThoughtChunk(ContentChunk {
            content: ContentBlock::Text(text),
            ..
        }) => Some(TurnEvent::AssistantThought { text: text.text }),
        SessionUpdate::ToolCall(tool_call) => Some(TurnEvent::ToolUse {
            name: tool_call.title,
            input: tool_call.raw_input.unwrap_or(Value::Null),
        }),
        // Drop status updates: the original ToolCall is already in the journal
        // and `completed`/`failed` carry no info the UI needs to render again.
        SessionUpdate::ToolCallUpdate(_) => None,
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
    pending_persona: Option<String>,
}

#[async_trait]
impl Handle for AcpHandle {
    async fn send(&mut self, prompt: &str) -> Result<(TurnStream, CancelSignal)> {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        self.cmd_tx
            .send(SessionCommand::Prompt {
                text: prompt.to_string(),
                event_tx,
                cancel_rx,
            })
            .map_err(|_| RoyError::ProcessExited)?;
        Ok((turn_stream(event_rx), cancel_tx))
    }

    fn resume_cursor(&self) -> Option<String> {
        Some(self.acp_sid.clone())
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.cmd_tx.send(SessionCommand::Close);
        Ok(())
    }

    fn take_pending_persona(&mut self) -> Option<String> {
        self.pending_persona.take()
    }
}

/// Stream the turn's events. Cancellation is signalled out-of-band via the
/// `CancelSignal` returned alongside the stream, so the stream stays connected
/// even after a cancel and still yields the terminal `Result` once the agent
/// winds down.
fn turn_stream(mut rx: mpsc::UnboundedReceiver<TurnEvent>) -> TurnStream {
    Box::pin(async_stream::stream! {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_preset_strips_claudecode_env() {
        // claude-code-acp refuses to launch if CLAUDECODE is set. The preset
        // must include it in env_remove so roy daemons launched from inside a
        // Claude Code session can still spawn claude children.
        let cfg = AcpConfig::claude();
        assert!(
            cfg.env_remove.iter().any(|k| k == "CLAUDECODE"),
            "AcpConfig::claude() must strip CLAUDECODE, got env_remove={:?}",
            cfg.env_remove
        );
    }

    #[test]
    fn other_presets_do_not_touch_env_by_default() {
        // Sanity: only the claude preset has env-stripping behaviour. If we
        // ever add to other presets, that decision deserves a deliberate test.
        assert!(AcpConfig::gemini().env_remove.is_empty());
        assert!(AcpConfig::opencode().env_remove.is_empty());
        assert!(AcpConfig::codex().env_remove.is_empty());
    }

    #[test]
    fn presets_declare_system_prompt_channel() {
        use super::SystemPromptChannel::*;
        assert_eq!(AcpConfig::claude().system_prompt_channel, Meta);
        assert_eq!(AcpConfig::opencode().system_prompt_channel, Meta);
        assert_eq!(AcpConfig::gemini().system_prompt_channel, FirstTurn);
        assert_eq!(AcpConfig::codex().system_prompt_channel, FirstTurn);
    }

    #[test]
    fn apply_system_prompt_meta_sets_append_key() {
        let mut meta = None;
        super::apply_system_prompt_meta(&mut meta, Some("PERSONA"));
        let m = meta.expect("meta set");
        assert_eq!(m["systemPrompt"]["append"], "PERSONA");
    }

    #[test]
    fn apply_system_prompt_meta_noop_when_none() {
        let mut meta = None;
        super::apply_system_prompt_meta(&mut meta, None);
        assert!(meta.is_none());
    }
}
