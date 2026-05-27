//! JSON-RPC 2.0 server loop. Speaks MCP to the parent ACP-agent process.
//!
//! One line in = one JSON-RPC message. Notifications (no `id`) produce no
//! response. Anything else returns either `result` or `error` framed in a
//! JSON-RPC 2.0 envelope, one per line on stdout.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::registry::Registry;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "roy-connections";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run(registry: Registry) -> Result<()> {
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = stdin_lines.next_line().await.context("reading stdin")? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_response(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &format!("parse error: {e}")),
                )
                .await?;
                continue;
            }
        };
        if let Some(resp) = handle_request(&req, &registry).await {
            write_response(&mut stdout, &resp).await?;
        }
    }
    registry.shutdown().await;
    Ok(())
}

async fn handle_request(req: &Value, registry: &Registry) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let id = req.get("id").cloned();
    let is_notification = id.is_none();

    match method {
        "initialize" if !is_notification => Some(initialize_result(id.unwrap_or(Value::Null))),
        "notifications/initialized" => None,
        "ping" if !is_notification => Some(ok_response(id.unwrap_or(Value::Null), json!({}))),
        "tools/list" if !is_notification => Some(ok_response(
            id.unwrap_or(Value::Null),
            json!({ "tools": registry.tools_list() }),
        )),
        "tools/call" if !is_notification => {
            let id = id.unwrap_or(Value::Null);
            let params = req.get("params").cloned().unwrap_or(json!({}));
            match registry.call_tool(params).await {
                Ok(value) => Some(ok_response(id, value)),
                Err(e) => Some(error_response(id, -32000, &e.to_string())),
            }
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
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        }
    })
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

async fn write_response<W: AsyncWriteExt + Unpin>(out: &mut W, v: &Value) -> Result<()> {
    let s = serde_json::to_string(v)?;
    out.write_all(s.as_bytes()).await?;
    out.write_all(b"\n").await?;
    out.flush().await?;
    Ok(())
}
