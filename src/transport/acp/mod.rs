pub mod client;
pub mod protocol;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Child;

use crate::error::{Result, RoyError};

use super::{owned_event_stream, Handle, StderrMode, Transport, TurnStream};
use client::JsonRpcClient;
pub use client::PermissionPolicy;

/// Launch + behaviour config for an ACP agent.
pub struct AcpConfig {
    pub command: String,
    pub args: Vec<String>,
    /// ACP mode to set after the session opens (e.g. "yolo" to auto-approve).
    pub mode_id: Option<String>,
    pub permission_policy: PermissionPolicy,
    pub request_timeout: Duration,
    pub stderr_mode: StderrMode,
}

impl AcpConfig {
    /// gemini --acp --skip-trust, auto-approving tools via yolo mode.
    pub fn gemini() -> Self {
        Self {
            command: "gemini".to_string(),
            args: vec!["--acp".to_string(), "--skip-trust".to_string()],
            mode_id: Some("yolo".to_string()),
            permission_policy: PermissionPolicy::AllowAll,
            request_timeout: Duration::from_secs(30),
            stderr_mode: StderrMode::Null,
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
            request_timeout: Duration::from_secs(30),
            stderr_mode: StderrMode::Null,
        }
    }

    /// codex via the Zed `@zed-industries/codex-acp` adapter (expects the
    /// `codex-acp` binary on PATH: `npm i -g @zed-industries/codex-acp`).
    /// `full-access` is the adapter's most permissive mode (analogous to
    /// gemini's yolo); residual permission requests are auto-allowed.
    pub fn codex() -> Self {
        Self {
            command: "codex-acp".to_string(),
            args: vec![],
            mode_id: Some("full-access".to_string()),
            permission_policy: PermissionPolicy::AllowAll,
            request_timeout: Duration::from_secs(30),
            stderr_mode: StderrMode::Null,
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
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(self.config.stderr_mode.stdio())
            .spawn()
            .map_err(|source| RoyError::Spawn {
                cmd: self.config.command.clone(),
                source,
            })?;

        let stdin = Box::new(child.stdin.take().expect("stdin piped"));
        let stdout = Box::new(child.stdout.take().expect("stdout piped"));
        let client = JsonRpcClient::new(
            stdout,
            stdin,
            self.config.permission_policy,
            self.config.request_timeout,
        );

        let open_result = async {
            client
                .request(
                    "initialize",
                    json!({"protocolVersion":1,"clientCapabilities":{}}),
                )
                .await?;

            let cwd_str = cwd.to_string_lossy().to_string();
            let acp_sid = match resume_cursor {
                Some(sid) => {
                    client
                        .request(
                            "session/load",
                            json!({"sessionId":sid,"cwd":cwd_str,"mcpServers":[]}),
                        )
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
                        .ok_or_else(|| {
                            RoyError::Protocol("session/new returned no sessionId".into())
                        })?
                }
            };

            if let Some(mode) = &self.config.mode_id {
                client
                    .request(
                        "session/set_mode",
                        json!({"sessionId":acp_sid,"modeId":mode}),
                    )
                    .await?;
            }

            Ok(acp_sid)
        }
        .await;

        let acp_sid = match open_result {
            Ok(acp_sid) => acp_sid,
            Err(err) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(err);
            }
        };

        Ok(Box::new(AcpHandle {
            child,
            client,
            acp_sid,
        }))
    }
}

pub struct AcpHandle {
    child: Child,
    client: Arc<JsonRpcClient>,
    acp_sid: String,
}

#[async_trait]
impl Handle for AcpHandle {
    async fn send(&mut self, prompt: &str) -> Result<TurnStream<'_>> {
        let params = json!({
            "sessionId": self.acp_sid,
            "prompt": [{"type":"text","text":prompt}]
        });
        let rx = self.client.begin_prompt(params).await?;
        Ok(owned_event_stream(rx))
    }

    fn resume_cursor(&self) -> Option<String> {
        Some(self.acp_sid.clone())
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        Ok(())
    }
}
