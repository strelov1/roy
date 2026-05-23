//! In-process registry of live `SessionEngine`s, keyed by session id.
//!
//! This is the single source of truth that future triggers (CLI Unix socket,
//! WebSocket, MCP server, ...) all talk to. See
//! `docs/superpowers/specs/2026-05-23-session-engine.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::engine::{EngineOpts, SessionEngine};
use crate::error::{Result, RoyError};
use crate::transport::Transport;

pub struct SessionManager {
    journal_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<SessionEngine>>>,
}

impl SessionManager {
    pub fn new(journal_dir: PathBuf) -> Self {
        Self {
            journal_dir,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Open a new session. The engine is spawned and registered before this
    /// returns; observers can `attach` immediately afterwards. Pass
    /// `resume_cursor = Some(cursor)` to ask the underlying transport to
    /// resume a prior agent-side session (e.g. via ACP `session/load`).
    pub async fn spawn(
        &self,
        transport: Arc<dyn Transport>,
        cwd: PathBuf,
        broadcast_capacity: usize,
        mem_capacity: usize,
        resume_cursor: Option<String>,
    ) -> Result<Arc<SessionEngine>> {
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine = SessionEngine::spawn(transport, cwd, opts, resume_cursor).await?;
        let id = engine.id().to_string();
        self.sessions.write().await.insert(id, Arc::clone(&engine));
        Ok(engine)
    }

    pub async fn list(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }

    pub async fn get(&self, id: &str) -> Option<Arc<SessionEngine>> {
        self.sessions.read().await.get(id).cloned()
    }

    /// Wind down a session: ask it to close and remove it from the registry.
    /// The journal file stays on disk for inspection.
    pub async fn close(&self, id: &str) -> Result<()> {
        let engine = self
            .sessions
            .write()
            .await
            .remove(id)
            .ok_or_else(|| RoyError::Protocol(format!("no such session: {id}")))?;
        engine.close()
    }

    pub fn journal_dir(&self) -> &PathBuf {
        &self.journal_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{AcpConfig, AcpTransport, PermissionPolicy};
    use std::time::Duration;

    fn fake_acp() -> AcpTransport {
        AcpTransport::new(AcpConfig {
            command: "python3".to_string(),
            args: vec!["tests/scripts/fake-acp-agent.py".to_string()],
            mode_id: Some("yolo".to_string()),
            permission_policy: PermissionPolicy::AllowAll,
            open_timeout: Duration::from_secs(5),
        })
    }

    static TMPDIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("roy-manager-test-{}-{n}", std::process::id()))
    }

    #[tokio::test]
    async fn registry_lifecycle() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(dir.clone());
        assert!(mgr.list().await.is_empty());

        let transport: Arc<dyn Transport> = Arc::new(fake_acp());
        let engine = mgr
            .spawn(transport, std::env::current_dir().unwrap(), 256, 1024, None)
            .await
            .unwrap();
        let id = engine.id().to_string();

        let ids = mgr.list().await;
        assert_eq!(ids, vec![id.clone()]);
        assert!(mgr.get(&id).await.is_some());

        mgr.close(&id).await.unwrap();
        assert!(mgr.list().await.is_empty());
        assert!(mgr.get(&id).await.is_none());
        assert!(mgr.close(&id).await.is_err(), "double close should error");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
