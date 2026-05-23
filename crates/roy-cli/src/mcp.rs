//! Minimal MCP (Model Context Protocol) server for the `roy mcp` subcommand.
//!
//! Speaks JSON-RPC 2.0 over stdio, exposes a small set of tools that map to
//! roy daemon control operations. Stateless: each tool call opens a fresh
//! Unix-socket connection to the daemon, drives one round trip, returns the
//! result.
//!
//! Spec reference: <https://modelcontextprotocol.io/specification/2024-11-05>.

use std::path::PathBuf;

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
                let resp = error_response(
                    Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                );
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

async fn dispatch(req: &Value, socket_path: &PathBuf) -> Option<Value> {
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
                        "agent": {"type": "string", "enum": ["claude_agent", "gemini", "opencode", "codex"]},
                        "task": {"type": "string"},
                        "cwd": {"type": "string"},
                        "model": {"type": "string"},
                        "permission": {"type": "string", "enum": ["allow", "deny"]},
                        "resume": {"type": "string", "description": "Agent-side resume cursor (e.g. prior ACP sessionId)."}
                    },
                    "required": ["agent", "task"],
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
            }
        ]
    })
}

async fn tools_call(id: Value, req: &Value, socket_path: &PathBuf) -> Value {
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match name {
        "roy_list_sessions" => tool_list(socket_path, false).await,
        "roy_list_archived" => tool_list(socket_path, true).await,
        "roy_run" => tool_run(socket_path, args).await,
        "roy_close" => tool_close(socket_path, args).await,
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
    socket_path: &PathBuf,
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

async fn next_event(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> anyhow::Result<ServerEvent> {
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("daemon hung up"))?;
    Ok(serde_json::from_str(line.trim())?)
}

async fn tool_list(socket_path: &PathBuf, archived: bool) -> anyhow::Result<String> {
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
                Ok(sessions.join("\n"))
            }
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_close(socket_path: &PathBuf, args: Value) -> anyhow::Result<String> {
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
        ServerEvent::Error { code, message, .. } => {
            Err(anyhow!("close failed: {code}: {message}"))
        }
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn tool_run(socket_path: &PathBuf, args: Value) -> anyhow::Result<String> {
    let agent = args
        .get("agent")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'agent'"))?
        .to_string();
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing 'task'"))?
        .to_string();
    let cwd = args.get("cwd").and_then(Value::as_str).map(|s| s.to_string());
    let model = args.get("model").and_then(Value::as_str).map(|s| s.to_string());
    let permission = args
        .get("permission")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let resume = args.get("resume").and_then(Value::as_str).map(|s| s.to_string());

    let (mut lines, mut writer) = open_daemon(socket_path).await?;

    // Spawn.
    send_cmd(
        &mut writer,
        &ClientCommand::Spawn {
            agent,
            cwd,
            model,
            permission,
            resume,
        },
    )
    .await?;
    let session = match next_event(&mut lines).await? {
        ServerEvent::Spawned { session, .. } => session,
        ServerEvent::Error { code, message, .. } => {
            return Err(anyhow!("spawn failed: {code}: {message}"))
        }
        other => return Err(anyhow!("unexpected response: {other:?}")),
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
        ServerEvent::InputAcquired { acquired: false, .. } => {
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
                TurnEvent::Result { stop_reason: sr, .. } => {
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

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, v: &Value) -> anyhow::Result<()> {
    let line = serde_json::to_string(v)?;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}
