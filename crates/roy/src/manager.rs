//! In-process registry of live `SessionEngine`s, keyed by session id.
//!
//! This is the single source of truth that future triggers (CLI Unix socket,
//! WebSocket, MCP server, ...) all talk to. The manager also owns the
//! `TransportFactory` so it can resurrect a session from its on-disk metadata
//! without the trigger having to remember which agent it was.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::daemon::TransportFactory;
use crate::engine::{EngineOpts, SessionEngine, SessionSpawnConfig};
use crate::error::{Result, RoyError};
use crate::session_meta::read_metadata;

pub struct SessionManager {
    journal_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<SessionEngine>>>,
    factory: Arc<dyn TransportFactory>,
}

impl SessionManager {
    pub fn new(journal_dir: PathBuf, factory: Arc<dyn TransportFactory>) -> Self {
        Self {
            journal_dir,
            sessions: RwLock::new(HashMap::new()),
            factory,
        }
    }

    /// Open a new session. The engine is spawned and registered before this
    /// returns; observers can `attach` immediately afterwards.
    pub async fn spawn(
        &self,
        cfg: SessionSpawnConfig,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Result<Arc<SessionEngine>> {
        let transport = self.factory.build(
            &cfg.agent,
            cfg.model.as_deref(),
            cfg.permission.as_deref(),
        )?;
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine = SessionEngine::spawn(transport, opts, cfg).await?;
        let id = engine.id().to_string();
        self.sessions.write().await.insert(id, Arc::clone(&engine));
        Ok(engine)
    }

    /// Resurrect a previously-closed (or restart-survived) session: read its
    /// metadata, rebuild the transport via the factory, and re-spawn the
    /// engine with the same id and journal.
    pub async fn resume(
        &self,
        session_id: &str,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Result<Arc<SessionEngine>> {
        if self.sessions.read().await.contains_key(session_id) {
            return Err(RoyError::Protocol(format!(
                "session {session_id} is already live"
            )));
        }
        let meta = read_metadata(&self.journal_dir, session_id).await?;
        let cfg = SessionSpawnConfig {
            agent: meta.agent,
            cwd: meta.cwd,
            model: meta.model,
            permission: meta.permission,
            resume_cursor: meta.resume_cursor,
        };
        let transport = self.factory.build(
            &cfg.agent,
            cfg.model.as_deref(),
            cfg.permission.as_deref(),
        )?;
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine =
            SessionEngine::resume(transport, opts, session_id.to_string(), cfg).await?;
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

    /// Session ids whose journal file exists on disk but whose engine is not
    /// in the live registry — e.g. closed sessions or survivors of a daemon
    /// restart. Returned in unspecified order.
    pub async fn list_archived(&self) -> Result<Vec<String>> {
        use std::collections::HashSet;
        let live: HashSet<String> = self.sessions.read().await.keys().cloned().collect();
        let mut archived = Vec::new();
        if !tokio::fs::try_exists(&self.journal_dir)
            .await
            .map_err(RoyError::Io)?
        {
            return Ok(archived);
        }
        let mut entries = tokio::fs::read_dir(&self.journal_dir)
            .await
            .map_err(RoyError::Io)?;
        while let Some(entry) = entries.next_entry().await.map_err(RoyError::Io)? {
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            let Some(id) = name.strip_suffix(".jsonl") else {
                continue;
            };
            if !live.contains(id) {
                archived.push(id.to_string());
            }
        }
        Ok(archived)
    }

    /// Open the journal for a session that is not (currently) live. Errors if
    /// either the session IS live (use `get` instead) or no journal file
    /// exists.
    pub async fn open_archive(
        &self,
        session_id: &str,
    ) -> Result<crate::journal::ArchivedJournal> {
        if self.sessions.read().await.contains_key(session_id) {
            return Err(RoyError::Protocol(format!(
                "session {session_id} is live — use `get` to attach, not `open_archive`"
            )));
        }
        crate::journal::ArchivedJournal::open(&self.journal_dir, session_id).await
    }

    /// Wind down a session: ask it to close and remove it from the registry.
    /// The journal file (and metadata) stay on disk for inspection / resume.
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

    pub fn factory(&self) -> &Arc<dyn TransportFactory> {
        &self.factory
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};
    use std::time::Duration;

    /// Test factory that always builds the fake ACP agent regardless of the
    /// requested agent name.
    struct FakeFactory;
    impl TransportFactory for FakeFactory {
        fn build(
            &self,
            _agent: &str,
            _model: Option<&str>,
            _permission: Option<&str>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(AcpTransport::new(AcpConfig {
                command: "python3".to_string(),
                args: vec!["tests/scripts/fake-acp-agent.py".to_string()],
                mode_id: Some("yolo".to_string()),
                permission_policy: PermissionPolicy::AllowAll,
                open_timeout: Duration::from_secs(5),
            })))
        }
    }

    static TMPDIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("roy-manager-test-{}-{n}", std::process::id()))
    }

    #[tokio::test]
    async fn registry_lifecycle() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(dir.clone(), Arc::new(FakeFactory));
        assert!(mgr.list().await.is_empty());

        let cfg = SessionSpawnConfig {
            agent: "opencode".into(),
            cwd: std::env::current_dir().unwrap(),
            model: None,
            permission: None,
            resume_cursor: None,
        };
        let engine = mgr.spawn(cfg, 256, 1024).await.unwrap();
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
