use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::error::{Result, RoyError};
use crate::event::TurnEvent;

use super::protocol::{prompt_result_to_event, update_to_event};

type Writer = Box<dyn AsyncWrite + Send + Unpin>;
type Reader = Box<dyn AsyncRead + Send + Unpin>;

struct Shared {
    pending: HashMap<i64, oneshot::Sender<std::result::Result<Value, Value>>>,
    turn_tx: Option<mpsc::Sender<TurnEvent>>,
    active_prompt_id: Option<i64>,
}

/// Minimal JSON-RPC 2.0 peer over a child's stdio. Handshake calls
/// (`request`) await their response; the prompt turn (`begin_prompt`) routes
/// `session/update` notifications and the terminal `session/prompt` result
/// into a per-turn channel.
pub struct JsonRpcClient {
    writer: Arc<Mutex<Writer>>,
    shared: Arc<Mutex<Shared>>,
    next_id: AtomicI64,
}

impl JsonRpcClient {
    pub fn new(reader: Reader, writer: Writer) -> Arc<Self> {
        let shared = Arc::new(Mutex::new(Shared {
            pending: HashMap::new(),
            turn_tx: None,
            active_prompt_id: None,
        }));
        let writer = Arc::new(Mutex::new(writer));
        let client = Arc::new(Self {
            writer: Arc::clone(&writer),
            shared: Arc::clone(&shared),
            next_id: AtomicI64::new(1),
        });

        let r_shared = Arc::clone(&shared);
        let r_writer = Arc::clone(&writer);
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                Self::route(&r_shared, &r_writer, msg).await;
            }
            // stdout closed: end any active turn.
            let mut s = r_shared.lock().await;
            s.turn_tx = None;
            s.active_prompt_id = None;
        });

        client
    }

    async fn route(shared: &Arc<Mutex<Shared>>, writer: &Arc<Mutex<Writer>>, msg: Value) {
        let id = msg.get("id").and_then(Value::as_i64);
        let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
        let has_result = msg.get("result").is_some() || msg.get("error").is_some();

        // Response to one of our requests.
        if let (Some(id), true) = (id, has_result) {
            let mut s = shared.lock().await;
            if s.active_prompt_id == Some(id) {
                s.active_prompt_id = None;
                let ev = msg
                    .get("result")
                    .map(prompt_result_to_event)
                    .unwrap_or(TurnEvent::Result { cost_usd: None, is_error: true });
                let tx = s.turn_tx.take();
                drop(s);
                if let Some(tx) = tx {
                    let _ = tx.send(ev).await;
                }
            } else if let Some(send) = s.pending.remove(&id) {
                if let Some(err) = msg.get("error") {
                    let _ = send.send(Err(err.clone()));
                } else {
                    let _ = send.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                }
            }
            return;
        }

        // Incoming agent->client request.
        if let (Some(id), Some(method)) = (id, method.clone()) {
            let response = if method.contains("request_permission") {
                json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})
            } else {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}})
            };
            let mut w = writer.lock().await;
            let _ = w.write_all(format!("{response}\n").as_bytes()).await;
            let _ = w.flush().await;
            return;
        }

        // Notification.
        if method.as_deref() == Some("session/update") {
            if let Some(params) = msg.get("params") {
                if let Some(ev) = update_to_event(params) {
                    let tx = { shared.lock().await.turn_tx.clone() };
                    if let Some(tx) = tx {
                        let _ = tx.send(ev).await;
                    }
                }
            }
        }
    }

    async fn write_msg(&self, msg: Value) -> Result<()> {
        let mut w = self.writer.lock().await;
        w.write_all(format!("{msg}\n").as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    /// Send a request and await its response. Used for the open handshake.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            self.shared.lock().await.pending.insert(id, tx);
        }
        self.write_msg(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(RoyError::Protocol(e.to_string())),
            Err(_) => Err(RoyError::ProcessExited),
        }
    }

    /// Begin a turn: install the event sink and fire `session/prompt`. Returns
    /// the receiver the caller streams until `TurnEvent::Result`.
    pub async fn begin_prompt(&self, params: Value) -> Result<mpsc::Receiver<TurnEvent>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel::<TurnEvent>(256);
        {
            let mut s = self.shared.lock().await;
            s.turn_tx = Some(tx);
            s.active_prompt_id = Some(id);
        }
        self.write_msg(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":params}))
            .await?;
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // Drives the client against an in-memory "agent" over duplex pipes.
    #[tokio::test]
    async fn request_correlates_response_by_id() {
        let (client_side, agent_side) = tokio::io::duplex(8192);
        let (agent_read, agent_write) = tokio::io::split(agent_side);
        let (client_read, client_write) = tokio::io::split(client_side);
        let client = JsonRpcClient::new(Box::new(client_read), Box::new(client_write));

        // Fake agent: read one request, reply with a result echoing its id.
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();
            let mut w = agent_write;
            if let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].as_i64().unwrap();
                let resp = json!({"jsonrpc":"2.0","id":id,"result":{"ok":true}});
                w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                w.flush().await.unwrap();
            }
        });

        let res = client.request("initialize", json!({"protocolVersion":1})).await.unwrap();
        assert_eq!(res["ok"], true);
    }

    #[tokio::test]
    async fn begin_prompt_streams_updates_then_result() {
        let (client_side, agent_side) = tokio::io::duplex(8192);
        let (agent_read, agent_write) = tokio::io::split(agent_side);
        let (client_read, client_write) = tokio::io::split(client_side);
        let client = JsonRpcClient::new(Box::new(client_read), Box::new(client_write));

        // Fake agent: on the prompt request, emit one update notification then
        // the terminal result with the same id.
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();
            let mut w = agent_write;
            if let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].as_i64().unwrap();
                let upd = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ack"}}}});
                w.write_all(format!("{upd}\n").as_bytes()).await.unwrap();
                let done = json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}});
                w.write_all(format!("{done}\n").as_bytes()).await.unwrap();
                w.flush().await.unwrap();
            }
        });

        let mut rx = client.begin_prompt(json!({"sessionId":"s","prompt":[]})).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            let end = matches!(ev, TurnEvent::Result { .. });
            got.push(ev);
            if end {
                break;
            }
        }
        assert!(got.iter().any(|e| matches!(e, TurnEvent::AssistantText { text } if text == "ack")));
        assert!(matches!(got.last(), Some(TurnEvent::Result { is_error: false, .. })));
    }
}
