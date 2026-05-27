//! One upstream MCP child process. Speaks JSON-RPC 2.0 over the child's
//! stdin/stdout. Stateless except for an autoincrementing request id.
//!
//! Concurrency: a single reader task drains stdout into a `pending` map keyed
//! by request id; writers acquire a Mutex on the child's stdin to write one
//! line at a time. Notifications from the upstream are dropped in MVP (no
//! `tools/list_changed` propagation — that's a follow-up).

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

use super::spec::ConnectionSpec;

pub struct Upstream {
    pub slug: String,
    child: Mutex<Child>,
    writer: Mutex<tokio::process::ChildStdin>,
    next_id: Mutex<i64>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    /// Cached tool list captured at startup. `Vec<Value>` (not a typed
    /// struct) so we can pass tool descriptors through to the aggregator
    /// without re-serializing.
    pub tools: Vec<Value>,
}

impl Upstream {
    /// Spawn the child, run `initialize` + `notifications/initialized` +
    /// `tools/list`. Returns an `Upstream` with the cached tool list.
    pub async fn start(spec: &ConnectionSpec) -> Result<Self> {
        if spec.kind != "mcp_stdio" {
            return Err(anyhow!(
                "upstream kind '{}' not supported (mcp_stdio only)",
                spec.kind
            ));
        }
        let cfg = &spec.config;
        let command = cfg
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("connection '{}': config.command missing", spec.slug))?;
        let args: Vec<String> = cfg
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let env_pairs: Vec<(String, String)> = cfg
            .get("env")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let secret_env: Vec<(String, String)> = spec
            .secrets
            .as_ref()
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let mut cmd = Command::new(command);
        cmd.args(&args)
            .envs(env_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .envs(secret_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning upstream '{}': {}", spec.slug, command))?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_for_reader = Arc::clone(&pending);
        let reader_slug = spec.slug.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(id) = v.get("id").and_then(Value::as_i64) {
                    if let Some(tx) = pending_for_reader.lock().await.remove(&id) {
                        let _ = tx.send(v);
                    }
                }
                // Notifications from upstream are dropped in MVP. C5+ may
                // re-emit `tools/list_changed` to the agent via the proxy.
            }
            tracing::info!(slug = %reader_slug, "upstream reader exited");
        });

        let mut up = Upstream {
            slug: spec.slug.clone(),
            child: Mutex::new(child),
            writer: Mutex::new(stdin),
            next_id: Mutex::new(1),
            pending,
            tools: Vec::new(),
        };

        up.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "roy-connections",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )
        .await
        .with_context(|| format!("initialize '{}'", spec.slug))?;
        up.notify("notifications/initialized", json!({})).await?;
        let tools_resp = up
            .request("tools/list", json!({}))
            .await
            .with_context(|| format!("tools/list '{}'", spec.slug))?;
        up.tools = tools_resp
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(up)
    }

    /// Forward a `tools/call` to the upstream.
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({"name": tool_name, "arguments": arguments}),
        )
        .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = {
            let mut n = self.next_id.lock().await;
            let cur = *n;
            *n += 1;
            cur
        };
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if let Err(e) = self.write_line(&req).await {
            // Write failed — the reader will never deliver a response for
            // this id, so reclaim the slot now.
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        let resp = rx.await.context("upstream closed before responding")?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("upstream error: {}", err));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let req = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write_line(&req).await
    }

    async fn write_line(&self, v: &Value) -> Result<()> {
        let s = serde_json::to_string(v)?;
        let mut w = self.writer.lock().await;
        w.write_all(s.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
        Ok(())
    }

    /// Kill the child. Idempotent.
    pub async fn shutdown(self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}
