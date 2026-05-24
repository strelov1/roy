//! Minimal MCP (Model Context Protocol) server for the `roy mcp` subcommand.
//!
//! Speaks JSON-RPC 2.0 over stdio, exposes a small set of tools that map to
//! roy daemon control operations. Stateless: each tool call opens a fresh
//! Unix-socket connection to the daemon, drives one round trip, returns the
//! result.
//!
//! Spec reference: <https://modelcontextprotocol.io/specification/2024-11-05>.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use roy::{ClientCommand, ServerEvent, TurnEvent};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "roy-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run(socket_path: PathBuf) -> anyhow::Result<()> {
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = stdin_lines
        .next_line()
        .await
        .context("reading from stdin")?
    {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let resp = error_response(Value::Null, -32700, &format!("parse error: {e}"));
                write_line(&mut stdout, &resp).await?;
                continue;
            }
        };
        if let Some(resp) = dispatch(&req, &socket_path).await {
            write_line(&mut stdout, &resp).await?;
        }
        // No response = notification (e.g. notifications/initialized).
    }
    Ok(())
}

async fn dispatch(req: &Value, socket_path: &Path) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let id = req.get("id").cloned();
    // JSON-RPC notifications have no `id`; they expect no response.
    let is_notification = id.is_none();

    match method {
        "initialize" => Some(initialize_result(id.unwrap_or(Value::Null))),
        "notifications/initialized" => {
            // Notification — no response.
            None
        }
        "ping" if !is_notification => Some(ok_result(id.unwrap_or(Value::Null), json!({}))),
        "tools/list" if !is_notification => {
            Some(ok_result(id.unwrap_or(Value::Null), tools_list()))
        }
        "tools/call" if !is_notification => {
            Some(tools_call(id.unwrap_or(Value::Null), req, socket_path).await)
        }
        _ if is_notification => None,
        _ => Some(error_response(
            id.unwrap_or(Value::Null),
            -32601,
            &format!("method not found: {method}"),
        )),
    }
}

fn initialize_result(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION,
            }
        }
    })
}

fn ok_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "roy_list_sessions",
                "description": "List live agent sessions managed by the roy daemon.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            },
            {
                "name": "roy_list_archived",
                "description": "List sessions whose journal files exist on disk but are not currently live (closed sessions or daemon-restart survivors).",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            },
            {
                "name": "roy_run",
                "description": "Spawn an agent session, send one task, wait for the turn to finish, and return the agent's concatenated text plus the stop reason. Suitable for short, synchronous tasks.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {"type": "string", "enum": ["claude", "gemini", "opencode", "codex"]},
                        "task": {"type": "string"},
                        "project_id": {"type": "string", "description": "Roy project id to run the session under. Omit to create an orphan session."},
                        "model": {"type": "string"},
                        "permission": {"type": "string", "enum": ["allow", "deny"]},
                        "resume": {"type": "string", "description": "Agent-side resume cursor (e.g. prior ACP sessionId)."}
                    },
                    "required": ["agent", "task"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_run_detached",
                "description": "Spawn an agent session, queue a task, and return immediately with the new session id. The session keeps running on the daemon — use roy_read_session to poll its progress and roy_close when done. Use this for long-running background tasks.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {"type": "string", "enum": ["claude", "gemini", "opencode", "codex"]},
                        "task": {"type": "string"},
                        "project_id": {"type": "string", "description": "Roy project id to run the session under. Omit to create an orphan session."},
                        "model": {"type": "string"},
                        "permission": {"type": "string", "enum": ["allow", "deny"]},
                        "resume": {"type": "string"}
                    },
                    "required": ["agent", "task"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_read_session",
                "description": "Snapshot read of a session's journal (live OR archived). Returns assistant text, tool calls, and stop reasons between `from_seq` and `next_seq`. Call again with the returned next_seq to keep polling. `max_entries` bounds the slice size; if truncated, `has_more` is true.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session": {"type": "string"},
                        "from_seq": {"type": "integer", "minimum": 0},
                        "max_entries": {"type": "integer", "minimum": 1}
                    },
                    "required": ["session"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_close",
                "description": "Ask the daemon to close a live session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"session": {"type": "string"}},
                    "required": ["session"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_set_tags",
                "description": "Replace the tag map on a live session. Pass an empty `tags` object to clear all tags.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session": {"type": "string"},
                        "tags": {"type": "object", "additionalProperties": {"type": "string"}}
                    },
                    "required": ["session", "tags"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_wait_for_result",
                "description": "Long-poll for the next terminal Result on a session. Returns when a turn finishes; emits a `wait_timeout` payload after `timeout_ms` (default 600000 = 10 min).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session": {"type": "string"},
                        "since_seq": {"type": "integer", "minimum": 0},
                        "timeout_ms": {"type": "integer", "minimum": 1}
                    },
                    "required": ["session"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_fire",
                "description": "One-shot: Spawn (or Resume) a session, send a prompt, wait for the terminal Result. Returns assistant_text + stop_reason. Pass `resume` to reuse an existing session id, otherwise pass `agent` (and optional `project_id`). Pass `parent` to record the caller's session id on the fire as the reserved tag `roy-scheduler:initiated_by_session`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {"type": "string", "enum": ["claude", "gemini", "opencode", "codex"]},
                        "project_id": {"type": "string", "description": "Roy project id to run the session under. Omit to create an orphan session."},
                        "resume": {"type": "string", "description": "Existing roy session id to resume into."},
                        "prompt": {"type": "string"},
                        "tags": {"type": "object", "additionalProperties": {"type": "string"}},
                        "parent": {"type": "string", "description": "Session id of the caller. Recorded on the fire as `roy-scheduler:initiated_by_session` so the UI can link back to the initiator."},
                        "timeout_ms": {"type": "integer", "minimum": 1}
                    },
                    "required": ["prompt"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_list_projects",
                "description": "List all projects in the roy registry. Each project has an id (UUID), a display name, the canonical filesystem path, and a created_at timestamp.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            },
            {
                "name": "roy_create_project",
                "description": "Create a new roy project with the given name. Roy manages the directory at `<workspace>/<name>/`. Returns the new project's id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Project name: ASCII letters, digits, '_', '-' only; no leading dot."}
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_delete_project",
                "description": "Cascade-delete a project and every session it owns. Permanently removes journal + metadata files. Returns the list of deleted session ids.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"project_id": {"type": "string"}},
                    "required": ["project_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "roy_list_agents",
                "description": "List agents configured in ~/.config/roy/agents.toml with their available models. Use to discover what `agent` string and `model` id values are valid for roy_run.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            }
        ]
    })
}

async fn tools_call(id: Value, req: &Value, socket_path: &Path) -> Value {
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match name {
        "roy_list_sessions" => tool_list(socket_path, false).await,
        "roy_list_archived" => tool_list(socket_path, true).await,
        "roy_run" => tool_run(socket_path, args).await,
        "roy_run_detached" => tool_run_detached(socket_path, args).await,
        "roy_read_session" => tool_read_session(socket_path, args).await,
        "roy_close" => tool_close(socket_path, args).await,
        "roy_set_tags" => tool_set_tags(socket_path, args).await,
        "roy_wait_for_result" => tool_wait_for_result(socket_path, args).await,
        "roy_fire" => tool_fire(socket_path, args).await,
        "roy_list_projects" => tool_list_projects(socket_path).await,
        "roy_create_project" => tool_create_project(socket_path, args).await,
        "roy_delete_project" => tool_delete_project(socket_path, args).await,
        "roy_list_agents" => tool_list_agents(socket_path).await,
        other => Err(anyhow!("unknown tool: {other}")),
    };

    match result {
        Ok(text) => ok_result(
            id,
            json!({
                "content": [{"type": "text", "text": text}],
                "isError": false
            }),
        ),
        Err(e) => ok_result(
            id,
            json!({
                "content": [{"type": "text", "text": format!("{e:#}")}],
                "isError": true
            }),
        ),
    }
}

/// Open a daemon connection, return (reader-lines, writer-half) ready for one
/// command exchange.
async fn open_daemon(
    socket_path: &Path,
) -> anyhow::Result<(
    tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    tokio::net::unix::OwnedWriteHalf,
)> {
    let stream = UnixStream::connect(socket_path).await.with_context(|| {
        format!(
            "no daemon at {} — start it with `roy serve`",
            socket_path.display()
        )
    })?;
    let (reader, writer) = stream.into_split();
    let lines = BufReader::new(reader).lines();
    Ok((lines, writer))
}

async fn send_cmd(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    cmd: &ClientCommand,
) -> anyhow::Result<()> {
    let line = serde_json::to_string(cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Shared shape of arguments for `roy_run` / `roy_run_detached`. Both tools
/// queue a `Spawn` with the same fields; only the post-spawn behavior differs.
#[derive(Debug)]
struct SpawnArgs {
    agent: String,
    task: String,
    project_id: Option<String>,
    model: Option<String>,
    permission: Option<String>,
    resume: Option<String>,
}

fn parse_spawn_args(args: &Value) -> anyhow::Result<SpawnArgs> {
    let required = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("missing '{k}'"))
    };
    let optional = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(SpawnArgs {
        agent: required("agent")?,
        task: required("task")?,
        project_id: optional("project_id"),
        model: optional("model"),
        permission: optional("permission"),
        resume: optional("resume"),
    })
}

async fn next_event(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> anyhow::Result<ServerEvent> {
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("daemon hung up"))?;
    Ok(serde_json::from_str(line.trim())?)
}

async fn tool_list(socket_path: &Path, archived: bool) -> anyhow::Result<String> {
    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    let cmd = if archived {
        ClientCommand::ListArchived
    } else {
        ClientCommand::List
    };
    send_cmd(&mut writer, &cmd).await?;
    match next_event(&mut lines).await? {
        ServerEvent::Listed { sessions } | ServerEvent::ListedArchived { sessions } => {
            if sessions.is_empty() {
                Ok("(no sessions)".to_string())
            } else {
                Ok(sessions
                    .iter()
                    .map(|s| s.session.clone())
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_close(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'session' argument"))?
        .to_string();
    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::Close {
            session: session.clone(),
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::Closed { .. } => Ok(format!("closed {session}")),
        ServerEvent::Error { code, message, .. } => Err(anyhow!("close failed: {code}: {message}")),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_set_tags(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'session' argument"))?
        .to_string();
    let mut tags = BTreeMap::new();
    if let Some(obj) = args.get("tags").and_then(Value::as_object) {
        for (k, v) in obj {
            let val = v
                .as_str()
                .ok_or_else(|| anyhow!("tag values must be strings, got non-string for `{k}`"))?;
            tags.insert(k.clone(), val.to_string());
        }
    } else {
        return Err(anyhow!("missing 'tags' object"));
    }

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::SetTags {
            session: session.clone(),
            tags,
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::SessionUpdated {
            session,
            tags: Some(t),
            ..
        } => Ok(serde_json::to_string(
            &json!({"session": session, "tags": t}),
        )?),
        ServerEvent::Error { code, message, .. } => {
            Err(anyhow!("set-tags failed: {code}: {message}"))
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_wait_for_result(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'session' argument"))?
        .to_string();
    let since_seq = args.get("since_seq").and_then(Value::as_u64);
    let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64);

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::WaitForResult {
            session: session.clone(),
            since_seq,
            timeout_ms,
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::ResultReady {
            session,
            seq,
            result,
            assistant_text,
        } => {
            let TurnEvent::Result {
                cost_usd,
                stop_reason,
            } = result
            else {
                return Err(anyhow!("non-Result in ResultReady"));
            };
            Ok(serde_json::to_string(&json!({
                "type": "result_ready",
                "session": session,
                "seq": seq,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            }))?)
        }
        ServerEvent::WaitTimeout { session } => Ok(serde_json::to_string(&json!({
            "type": "wait_timeout",
            "session": session,
        }))?),
        ServerEvent::Error { code, message, .. } => {
            Err(anyhow!("wait_for_result failed: {code}: {message}"))
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_fire(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    use roy::FireTarget;

    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'prompt'"))?
        .to_string();
    let agent = args.get("agent").and_then(Value::as_str);
    let project_id = args
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let resume = args.get("resume").and_then(Value::as_str);
    let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64);

    let target = match (agent, resume) {
        (Some(a), None) => FireTarget::Spawn {
            preset: a.to_string(),
            project_id,
        },
        (None, Some(sid)) => FireTarget::Resume {
            session_id: sid.to_string(),
        },
        (Some(_), Some(_)) => return Err(anyhow!("`agent` and `resume` are mutually exclusive")),
        (None, None) => return Err(anyhow!("provide either `agent` or `resume`")),
    };

    let mut tags = BTreeMap::new();
    if let Some(obj) = args.get("tags").and_then(Value::as_object) {
        for (k, v) in obj {
            let val = v
                .as_str()
                .ok_or_else(|| anyhow!("tag value for `{k}` must be string"))?;
            tags.insert(k.clone(), val.to_string());
        }
    }
    // `parent` records the caller's session id as the reserved tag
    // `roy-scheduler:initiated_by_session` (distinct from
    // `roy-scheduler:parent_session_id`, which the inject_parent subscriber
    // owns). Inserted unconditionally so callers can't quietly silence the
    // link by also passing `tags["roy-scheduler:initiated_by_session"]`.
    if let Some(parent) = args.get("parent").and_then(Value::as_str) {
        tags.insert(
            "roy-scheduler:initiated_by_session".to_string(),
            parent.to_string(),
        );
    }

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::Fire {
            target,
            prompt,
            tags,
            timeout_ms,
        },
    )
    .await?;

    match next_event(&mut lines).await? {
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
            Ok(serde_json::to_string(&json!({
                "type": "fire_done",
                "session": session,
                "seq_range": seq_range,
                "stop_reason": format!("{stop_reason:?}"),
                "cost_usd": cost_usd,
                "assistant_text": assistant_text,
            }))?)
        }
        ServerEvent::FireTimeout {
            session,
            partial_seq_range,
        } => Ok(serde_json::to_string(&json!({
            "type": "fire_timeout",
            "session": session,
            "partial_seq_range": partial_seq_range,
        }))?),
        ServerEvent::FireError {
            session,
            code,
            message,
        } => Ok(serde_json::to_string(&json!({
            "type": "fire_error",
            "session": session,
            "code": code.to_string(),
            "message": message,
        }))?),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_run(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let SpawnArgs {
        agent,
        task,
        project_id,
        model,
        permission,
        resume,
    } = parse_spawn_args(&args)?;

    let (mut lines, mut writer) = open_daemon(socket_path).await?;

    // Spawn.
    send_cmd(
        &mut writer,
        &ClientCommand::Spawn {
            agent,
            project_id,
            model,
            permission,
            resume,
            tags: BTreeMap::default(),
        },
    )
    .await?;
    let session = loop {
        match next_event(&mut lines).await? {
            ServerEvent::Spawning { .. } => continue,
            ServerEvent::Spawned { session, .. } => break session,
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("spawn failed: {code}: {message}"))
            }
            other => return Err(anyhow!("unexpected response: {other:?}")),
        }
    };

    // Attach before send so no frames are missed.
    send_cmd(
        &mut writer,
        &ClientCommand::Attach {
            session: session.clone(),
            from_seq: None,
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::Attached { .. } => {}
        ServerEvent::Error { code, message, .. } => {
            return Err(anyhow!("attach failed: {code}: {message}"))
        }
        other => return Err(anyhow!("unexpected response: {other:?}")),
    }

    // Acquire input + send.
    send_cmd(
        &mut writer,
        &ClientCommand::AcquireInput {
            session: session.clone(),
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::InputAcquired { acquired: true, .. } => {}
        ServerEvent::InputAcquired {
            acquired: false, ..
        } => {
            return Err(anyhow!("input lease already held"));
        }
        other => return Err(anyhow!("unexpected response: {other:?}")),
    }
    send_cmd(
        &mut writer,
        &ClientCommand::Send {
            session: session.clone(),
            text: task,
        },
    )
    .await?;

    // Drain frames until terminal Result; concatenate assistant text and
    // capture the stop reason as the loop's value.
    let mut text = String::new();
    let stop_reason: String = loop {
        match next_event(&mut lines).await? {
            ServerEvent::Frame { entry, .. } => match entry.event {
                TurnEvent::AssistantText { text: chunk } => text.push_str(&chunk),
                TurnEvent::Result {
                    stop_reason: sr, ..
                } => {
                    break sr.as_wire().to_string();
                }
                _ => {}
            },
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("session error: {code}: {message}"))
            }
            _ => {}
        }
    };

    // Close — `run` semantics are one-shot.
    send_cmd(
        &mut writer,
        &ClientCommand::Close {
            session: session.clone(),
        },
    )
    .await?;
    let _ = next_event(&mut lines).await;

    Ok(format!("{text}\n[stop_reason: {stop_reason}]"))
}

async fn tool_run_detached(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let SpawnArgs {
        agent,
        task,
        project_id,
        model,
        permission,
        resume,
    } = parse_spawn_args(&args)?;

    let (mut lines, mut writer) = open_daemon(socket_path).await?;

    send_cmd(
        &mut writer,
        &ClientCommand::Spawn {
            agent,
            project_id,
            model,
            permission,
            resume,
            tags: BTreeMap::default(),
        },
    )
    .await?;
    let (session, resume_cursor) = loop {
        match next_event(&mut lines).await? {
            ServerEvent::Spawning { .. } => continue,
            ServerEvent::Spawned {
                session,
                resume_cursor,
                ..
            } => break (session, resume_cursor),
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("spawn failed: {code}: {message}"))
            }
            other => return Err(anyhow!("unexpected response: {other:?}")),
        }
    };

    // Acquire + send so the prompt is queued before we drop the lease.
    send_cmd(
        &mut writer,
        &ClientCommand::AcquireInput {
            session: session.clone(),
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::InputAcquired { acquired: true, .. } => {}
        ServerEvent::InputAcquired {
            acquired: false, ..
        } => return Err(anyhow!("input lease already held")),
        other => return Err(anyhow!("unexpected response: {other:?}")),
    }
    send_cmd(
        &mut writer,
        &ClientCommand::Send {
            session: session.clone(),
            text: task,
        },
    )
    .await?;
    // No drain: dropping this connection releases the lease automatically;
    // the engine actor keeps processing the queued prompt.

    let payload = json!({
        "session_id": session,
        "resume_cursor": resume_cursor,
    });
    Ok(serde_json::to_string_pretty(&payload)?)
}

async fn tool_read_session(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'session'"))?
        .to_string();
    let from_seq = args.get("from_seq").and_then(Value::as_u64);
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(
        &mut writer,
        &ClientCommand::ReadJournal {
            session: session.clone(),
            from_seq,
            max_entries,
        },
    )
    .await?;
    match next_event(&mut lines).await? {
        ServerEvent::JournalRead {
            entries,
            next_seq,
            has_more,
            ..
        } => {
            // Render entries as a compact, LLM-friendly summary plus the
            // structured next_seq for follow-up polling.
            let mut rendered = Vec::with_capacity(entries.len());
            let mut terminal: Option<String> = None;
            for entry in &entries {
                match &entry.event {
                    TurnEvent::UserPrompt { text } => {
                        rendered.push(format!("[{}] user: {text}", entry.seq));
                    }
                    TurnEvent::AssistantText { text } => {
                        rendered.push(format!("[{}] assistant: {text}", entry.seq));
                    }
                    TurnEvent::AssistantThought { text } => {
                        rendered.push(format!("[{}] thought: {text}", entry.seq));
                    }
                    TurnEvent::Usage {
                        input_tokens,
                        output_tokens,
                        cost_usd,
                    } => {
                        let mut parts = Vec::new();
                        if let Some(n) = input_tokens {
                            parts.push(format!("in={n}"));
                        }
                        if let Some(n) = output_tokens {
                            parts.push(format!("out={n}"));
                        }
                        if let Some(c) = cost_usd {
                            parts.push(format!("cost=${c:.4}"));
                        }
                        rendered.push(format!("[{}] usage: {}", entry.seq, parts.join(" ")));
                    }
                    TurnEvent::ToolUse { name, .. } => {
                        rendered.push(format!("[{}] tool_use: {name}", entry.seq));
                    }
                    TurnEvent::System { subtype } => {
                        rendered.push(format!("[{}] system: {subtype}", entry.seq));
                    }
                    TurnEvent::Result { stop_reason, .. } => {
                        rendered.push(format!("[{}] result: {}", entry.seq, stop_reason.as_wire()));
                        terminal = Some(stop_reason.as_wire().to_string());
                    }
                    TurnEvent::Raw(_) => {
                        rendered.push(format!("[{}] raw", entry.seq));
                    }
                }
            }
            let body = rendered.join("\n");
            let footer = format!(
                "\n[next_seq={next_seq} has_more={has_more}{}]",
                terminal
                    .map(|s| format!(" stop_reason={s}"))
                    .unwrap_or_default()
            );
            Ok(if body.is_empty() {
                format!("(no new entries){footer}")
            } else {
                format!("{body}{footer}")
            })
        }
        ServerEvent::Error { code, message, .. } => Err(anyhow!("read failed: {code}: {message}")),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_list_projects(socket_path: &Path) -> anyhow::Result<String> {
    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(&mut writer, &ClientCommand::ListProjects).await?;
    match next_event(&mut lines).await? {
        ServerEvent::ProjectsListed { projects } => Ok(serde_json::to_string(&projects)?),
        ServerEvent::Error { code, message, .. } => Err(anyhow!("{code}: {message}")),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_create_project(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required field: name"))?
        .to_string();

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(&mut writer, &ClientCommand::CreateProject { name }).await?;
    match next_event(&mut lines).await? {
        ServerEvent::ProjectCreated { project } => Ok(serde_json::to_string(&project)?),
        ServerEvent::Error { code, message, .. } => Err(anyhow!("{code}: {message}")),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_delete_project(socket_path: &Path, args: Value) -> anyhow::Result<String> {
    let project_id = args
        .get("project_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required field: project_id"))?
        .to_string();

    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(&mut writer, &ClientCommand::DeleteProject { project_id }).await?;
    match next_event(&mut lines).await? {
        ServerEvent::ProjectDeleted {
            project_id,
            deleted_sessions,
        } => Ok(serde_json::to_string(&json!({
            "project_id": project_id,
            "deleted_sessions": deleted_sessions,
        }))?),
        ServerEvent::Error { code, message, .. } => Err(anyhow!("{code}: {message}")),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_list_agents(socket_path: &Path) -> anyhow::Result<String> {
    let (mut lines, mut writer) = open_daemon(socket_path).await?;
    send_cmd(&mut writer, &ClientCommand::ListAgents).await?;
    match next_event(&mut lines).await? {
        ServerEvent::AgentsList {
            agents,
            config_path,
            status,
        } => Ok(serde_json::to_string(&json!({
            "agents": agents,
            "config_path": config_path,
            "status": status,
        }))?),
        ServerEvent::Error { code, message, .. } => Err(anyhow!("{code}: {message}")),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, v: &Value) -> anyhow::Result<()> {
    let line = serde_json::to_string(v)?;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tools/list` must enumerate exactly the tools the LLM-facing surface
    /// promises. A drift between this set and the documented API would silently
    /// break MCP clients, so the test pins the list explicitly.
    #[test]
    fn tools_list_enumerates_the_documented_tools() {
        let list = tools_list();
        let names: Vec<&str> = list["tools"]
            .as_array()
            .expect("tools is an array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name is a string"))
            .collect();
        assert_eq!(
            names,
            vec![
                "roy_list_sessions",
                "roy_list_archived",
                "roy_run",
                "roy_run_detached",
                "roy_read_session",
                "roy_close",
                "roy_set_tags",
                "roy_wait_for_result",
                "roy_fire",
                "roy_list_projects",
                "roy_create_project",
                "roy_delete_project",
                "roy_list_agents",
            ]
        );
    }

    #[test]
    fn initialize_result_wraps_id_and_advertises_tools_capability() {
        let r = initialize_result(json!(42));
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], json!(42));
        assert_eq!(r["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(r["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(r["result"]["capabilities"]
            .as_object()
            .unwrap()
            .contains_key("tools"));
    }

    #[test]
    fn ok_and_error_response_match_jsonrpc_envelope() {
        let ok = ok_result(json!("abc"), json!({"foo": 1}));
        assert_eq!(ok["jsonrpc"], "2.0");
        assert_eq!(ok["id"], json!("abc"));
        assert_eq!(ok["result"], json!({"foo": 1}));

        let err = error_response(json!(7), -32601, "method not found: xyz");
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], json!(7));
        assert_eq!(err["error"]["code"], json!(-32601));
        assert_eq!(err["error"]["message"], json!("method not found: xyz"));
    }

    #[test]
    fn parse_spawn_args_extracts_required_and_optional_fields() {
        let parsed = parse_spawn_args(&json!({
            "agent": "opencode",
            "task": "do it",
            "project_id": "pid-123",
            "model": "gpt-x",
            "permission": "allow",
            "resume": "sid-1"
        }))
        .unwrap();
        assert_eq!(parsed.agent, "opencode");
        assert_eq!(parsed.task, "do it");
        assert_eq!(parsed.project_id.as_deref(), Some("pid-123"));
        assert_eq!(parsed.model.as_deref(), Some("gpt-x"));
        assert_eq!(parsed.permission.as_deref(), Some("allow"));
        assert_eq!(parsed.resume.as_deref(), Some("sid-1"));
    }

    #[test]
    fn parse_spawn_args_omits_missing_optional_fields() {
        let parsed = parse_spawn_args(&json!({
            "agent": "gemini",
            "task": "go"
        }))
        .unwrap();
        assert_eq!(parsed.agent, "gemini");
        assert_eq!(parsed.task, "go");
        assert!(parsed.project_id.is_none());
        assert!(parsed.model.is_none());
        assert!(parsed.permission.is_none());
        assert!(parsed.resume.is_none());
    }

    #[test]
    fn parse_spawn_args_errors_when_required_fields_missing() {
        let err = parse_spawn_args(&json!({"task": "x"})).unwrap_err();
        assert!(err.to_string().contains("'agent'"));

        let err = parse_spawn_args(&json!({"agent": "gemini"})).unwrap_err();
        assert!(err.to_string().contains("'task'"));
    }

    /// `dispatch` must answer `initialize`, `tools/list`, `ping`, and unknown
    /// methods without ever touching the socket — these branches don't open a
    /// daemon connection, so an invalid path is a fine stand-in.
    #[tokio::test]
    async fn dispatch_initialize_returns_handshake_envelope() {
        let bogus = Path::new("/dev/null/not-a-socket");
        let resp = dispatch(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
            bogus,
        )
        .await
        .unwrap();
        assert_eq!(resp["id"], json!(1));
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn dispatch_tools_list_returns_full_inventory() {
        let bogus = Path::new("/dev/null/not-a-socket");
        let resp = dispatch(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            bogus,
        )
        .await
        .unwrap();
        assert_eq!(resp["id"], json!(2));
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"roy_run"));
        assert!(names.contains(&"roy_read_session"));
    }

    #[tokio::test]
    async fn dispatch_ping_returns_empty_ok() {
        let bogus = Path::new("/dev/null/not-a-socket");
        let resp = dispatch(&json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}), bogus)
            .await
            .unwrap();
        assert_eq!(resp["result"], json!({}));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let bogus = Path::new("/dev/null/not-a-socket");
        let resp = dispatch(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "totally/unknown"}),
            bogus,
        )
        .await
        .unwrap();
        assert_eq!(resp["error"]["code"], json!(-32601));
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("totally/unknown"));
    }

    #[tokio::test]
    async fn dispatch_notification_yields_no_response() {
        let bogus = Path::new("/dev/null/not-a-socket");
        // No `id` field → notification → must produce no response.
        let resp = dispatch(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            bogus,
        )
        .await;
        assert!(resp.is_none());
    }
}
