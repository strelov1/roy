//! Daemon-client abstraction: trait `DaemonClient` for management-side
//! coordination, plus the production `UnixSocketDaemonClient` impl. Tests
//! use `MockDaemonClient` (see `meta_store::tests`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use roy::{ClientCommand, ServerEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::meta_store::{MetaStore, SessionMeta};

#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub agent: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub system_prompt: Option<String>,
    pub extra_env: std::collections::HashMap<String, String>,
}

#[async_trait]
pub trait DaemonClient: Send + Sync {
    async fn spawn(&self, req: SpawnRequest) -> Result<String>;
    async fn close(&self, session_id: &str) -> Result<()>;
    async fn list(&self) -> Result<Vec<String>>;
    async fn list_archived(&self) -> Result<Vec<String>>;
    async fn list_presets(&self) -> Result<serde_json::Value>;
}

pub struct UnixSocketDaemonClient {
    socket: PathBuf,
}

impl UnixSocketDaemonClient {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    async fn connect_and_send(
        &self,
        cmd: &ClientCommand,
    ) -> Result<tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>> {
        connect_and_send(&self.socket, cmd).await
    }
}

#[async_trait]
impl DaemonClient for UnixSocketDaemonClient {
    async fn spawn(&self, req: SpawnRequest) -> Result<String> {
        // NOTE: After Phase 3 lands, `ClientCommand::Spawn` will have `cwd`
        // instead of `project_id`. This impl assumes that final shape.
        let cmd = ClientCommand::Spawn {
            agent: req.agent,
            cwd: req.cwd,
            model: req.model,
            permission: req.permission,
            resume: None,
            system_prompt: req.system_prompt,
            extra_env: req.extra_env,
        };
        let mut lines = self.connect_and_send(&cmd).await?;
        loop {
            let raw = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("daemon hung up before Spawned"))?;
            match serde_json::from_str::<ServerEvent>(raw.trim())? {
                ServerEvent::Spawning { .. } => continue,
                ServerEvent::Spawned { session, .. } => return Ok(session),
                ServerEvent::Error { code, message, .. } => {
                    return Err(anyhow!("daemon error [{code}]: {message}"))
                }
                _ => continue,
            }
        }
    }

    async fn close(&self, session_id: &str) -> Result<()> {
        let mut lines = self
            .connect_and_send(&ClientCommand::Close {
                session: session_id.into(),
            })
            .await?;
        loop {
            let raw = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("daemon hung up before Closed"))?;
            match serde_json::from_str::<ServerEvent>(raw.trim())? {
                ServerEvent::Closed { .. } => return Ok(()),
                ServerEvent::Error { code, message, .. } => {
                    return Err(anyhow!("daemon error [{code}]: {message}"))
                }
                _ => continue,
            }
        }
    }

    async fn list(&self) -> Result<Vec<String>> {
        list_inner(&self.socket, ClientCommand::List).await
    }

    async fn list_archived(&self) -> Result<Vec<String>> {
        list_inner(&self.socket, ClientCommand::ListArchived).await
    }

    async fn list_presets(&self) -> Result<serde_json::Value> {
        let mut lines = self.connect_and_send(&ClientCommand::ListAgents).await?;
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
}

async fn list_inner(socket: &Path, cmd: ClientCommand) -> Result<Vec<String>> {
    let mut lines = connect_and_send(socket, &cmd).await?;
    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up"))?;
        match serde_json::from_str::<ServerEvent>(raw.trim())? {
            ServerEvent::Listed { sessions } | ServerEvent::ListedArchived { sessions } => {
                return Ok(sessions.into_iter().map(|s| s.session).collect());
            }
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("daemon error [{code}]: {message}"))
            }
            _ => continue,
        }
    }
}

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

#[cfg(any(test, feature = "test-support"))]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    /// Configurable mock for HTTP-handler tests. Records all spawn/close
    /// calls and returns scripted responses.
    #[derive(Default)]
    pub struct MockDaemonClient {
        pub spawn_response: Mutex<Option<Result<String, String>>>,
        pub close_response: Mutex<Option<Result<(), String>>>,
        pub list_response: Mutex<Option<Vec<String>>>,
        pub recorded_spawns: Mutex<Vec<SpawnRequest>>,
        pub recorded_closes: Mutex<Vec<String>>,
    }

    impl MockDaemonClient {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_spawn(mut self, sid: &str) -> Self {
            self.spawn_response = Mutex::new(Some(Ok(sid.into())));
            self
        }

        /// Returns the most recent spawn request recorded by this mock.
        /// Panics if no spawn has been recorded yet — intended for tests that
        /// have already issued a request and want to assert on its shape.
        pub fn last_spawn(&self) -> SpawnRequest {
            self.recorded_spawns
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("no spawn recorded")
        }
    }

    #[async_trait]
    impl DaemonClient for MockDaemonClient {
        async fn spawn(&self, req: SpawnRequest) -> Result<String> {
            self.recorded_spawns.lock().unwrap().push(req);
            match self.spawn_response.lock().unwrap().take() {
                Some(Ok(s)) => Ok(s),
                Some(Err(e)) => Err(anyhow!(e)),
                None => Err(anyhow!("MockDaemonClient: no spawn_response set")),
            }
        }
        async fn close(&self, sid: &str) -> Result<()> {
            self.recorded_closes.lock().unwrap().push(sid.into());
            match self.close_response.lock().unwrap().take() {
                Some(Ok(())) => Ok(()),
                Some(Err(e)) => Err(anyhow!(e)),
                None => Ok(()),
            }
        }
        async fn list(&self) -> Result<Vec<String>> {
            Ok(self
                .list_response
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_default())
        }
        async fn list_archived(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_presets(&self) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
    }
}

// Preserve previous free-function for run_agent / start_builder until the HTTP
// migration in later tasks; will be removed when POST /agents/{id}/run and
// POST /agents/_builder go through /sessions. `tags` is persisted to
// `meta_store` after the daemon spawn so the UI can mark these sessions
// (e.g. builder sessions get a wrench icon in the sidebar). The daemon's
// wire protocol does not currently carry tags on `Spawn`; tags live entirely
// in management-side meta.
pub async fn spawn(
    socket: &Path,
    meta: &MetaStore,
    preset: &str,
    model: Option<String>,
    system_prompt: Option<String>,
    tags: BTreeMap<String, String>,
    created_by: &str,
) -> Result<String> {
    let session = UnixSocketDaemonClient::new(socket.to_path_buf())
        .spawn(SpawnRequest {
            agent: preset.into(),
            cwd: None,
            model,
            permission: None,
            system_prompt,
            extra_env: Default::default(),
        })
        .await?;
    // Persist tags into management-owned meta so `GET /sessions` returns
    // them. If this fails after the daemon spawned, the session leaks meta —
    // log and surface the error so the caller can decide how to handle it.
    // `created_by` is the authenticated user_id threaded from the HTTP handler;
    // the wire-format `SpawnRequest` carries no user identity (the daemon is
    // trusted), so ownership is recorded only in management-side meta.
    let row = SessionMeta {
        session_id: session.clone(),
        project_id: None,
        agent_id: None,
        agent_name: None,
        display_label: None,
        created_by: created_by.into(),
        team_id: None,
        tags,
        created_at: chrono::Utc::now().timestamp(),
    };
    meta.upsert_session_meta(&row)
        .await
        .with_context(|| format!("persisting tags for session {session}"))?;
    Ok(session)
}

pub async fn list_presets(socket: &Path) -> Result<serde_json::Value> {
    UnixSocketDaemonClient::new(socket.to_path_buf())
        .list_presets()
        .await
}
