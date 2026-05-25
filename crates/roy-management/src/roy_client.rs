//! Minimal roy daemon client: newline-delimited JSON `ClientCommand` →
//! `ServerEvent` over the Unix socket. Only the calls roy-management needs.
//! The socket is the ONLY way this crate touches the daemon (boundary rule).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use roy::{ClientCommand, ServerEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

async fn connect_and_send(
    socket: &Path,
    cmd: &ClientCommand,
) -> Result<tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to roy daemon at {}", socket.display()))?;
    let (reader, mut writer) = stream.into_split();
    let line = serde_json::to_string(cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(BufReader::new(reader).lines())
}

/// Spawn a session with an inline persona. Returns the new session id.
pub async fn spawn(
    socket: &Path,
    preset: &str,
    model: Option<String>,
    system_prompt: Option<String>,
) -> Result<String> {
    let cmd = ClientCommand::Spawn {
        agent: preset.to_string(),
        project_id: None,
        model,
        permission: None,
        resume: None,
        tags: BTreeMap::new(),
        system_prompt,
    };
    let mut lines = connect_and_send(socket, &cmd).await?;
    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up before Spawned"))?;
        match serde_json::from_str::<ServerEvent>(raw.trim())? {
            // `Spawning` is the pre-launch ack; keep reading for the terminal one.
            ServerEvent::Spawning { .. } => continue,
            ServerEvent::Spawned { session, .. } => return Ok(session),
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("daemon error [{code}]: {message}"))
            }
            _ => continue,
        }
    }
}

/// Fetch the preset+model catalog (`ListAgents`). Returns the raw `AgentsList`
/// event as a JSON value so the UI / caller can use it directly.
pub async fn list_presets(socket: &Path) -> Result<serde_json::Value> {
    let mut lines = connect_and_send(socket, &ClientCommand::ListAgents).await?;
    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up before AgentsList"))?;
        let trimmed = raw.trim();
        match serde_json::from_str::<ServerEvent>(trimmed)? {
            ServerEvent::AgentsList { .. } => return Ok(serde_json::from_str(trimmed)?),
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("daemon error [{code}]: {message}"))
            }
            _ => continue,
        }
    }
}
