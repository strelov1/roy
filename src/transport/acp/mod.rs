pub mod client;
pub mod protocol;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Child;
use tokio_stream::Stream;

use crate::error::{Result, RoyError};
use crate::event::TurnEvent;

use super::{Handle, Transport};
use client::JsonRpcClient;

/// Launch + behaviour config for an ACP agent.
pub struct AcpConfig {
    pub command: String,
    pub args: Vec<String>,
    /// ACP mode to set after the session opens (e.g. "yolo" to auto-approve).
    pub mode_id: Option<String>,
}

impl AcpConfig {
    /// gemini --acp --skip-trust, auto-approving tools via yolo mode.
    pub fn gemini() -> Self {
        Self {
            command: "gemini".to_string(),
            args: vec!["--acp".to_string(), "--skip-trust".to_string()],
            mode_id: Some("yolo".to_string()),
        }
    }

    /// opencode acp. OpenCode has no ACP "modes" (it exposes configOptions
    /// instead), so no set_mode is sent.
    pub fn opencode() -> Self {
        Self {
            command: "opencode".to_string(),
            args: vec!["acp".to_string()],
            mode_id: None,
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

#[async_trait]
impl Transport for AcpTransport {
    async fn open(
        &self,
        _session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>> {
        let mut child = tokio::process::Command::new(&self.config.command)
            .args(&self.config.args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| RoyError::Spawn {
                cmd: self.config.command.clone(),
                source,
            })?;

        let stdin = Box::new(child.stdin.take().expect("stdin piped"));
        let stdout = Box::new(child.stdout.take().expect("stdout piped"));
        let client = JsonRpcClient::new(stdout, stdin);

        client
            .request("initialize", json!({"protocolVersion":1,"clientCapabilities":{}}))
            .await?;

        let cwd_str = cwd.to_string_lossy().to_string();
        let acp_sid = match resume_cursor {
            Some(sid) => {
                client
                    .request("session/load", json!({"sessionId":sid,"cwd":cwd_str,"mcpServers":[]}))
                    .await?;
                sid.to_string()
            }
            None => {
                let res = client
                    .request("session/new", json!({"cwd":cwd_str,"mcpServers":[]}))
                    .await?;
                res.get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| RoyError::Protocol("session/new returned no sessionId".into()))?
            }
        };

        if let Some(mode) = &self.config.mode_id {
            client
                .request("session/set_mode", json!({"sessionId":acp_sid,"modeId":mode}))
                .await?;
        }

        Ok(Box::new(AcpHandle { child, client, acp_sid }))
    }
}

pub struct AcpHandle {
    child: Child,
    client: Arc<JsonRpcClient>,
    acp_sid: String,
}

#[async_trait]
impl Handle for AcpHandle {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>> {
        let params = json!({
            "sessionId": self.acp_sid,
            "prompt": [{"type":"text","text":prompt}]
        });
        let mut rx = self.client.begin_prompt(params).await?;
        let stream = async_stream::stream! {
            while let Some(ev) = rx.recv().await {
                let end = matches!(ev, TurnEvent::Result { .. });
                yield ev;
                if end {
                    break;
                }
            }
        };
        Ok(Box::pin(stream))
    }

    fn resume_cursor(&self) -> Option<String> {
        Some(self.acp_sid.clone())
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.child.start_kill();
        Ok(())
    }
}
