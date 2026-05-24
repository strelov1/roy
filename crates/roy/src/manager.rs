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
use crate::journal::{JournalEntry, Seq};
use crate::project::{Project, ProjectRegistry};
use crate::session_meta::read_metadata;

pub struct SessionManager {
    journal_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<SessionEngine>>>,
    factory: Arc<dyn TransportFactory>,
    projects: Arc<ProjectRegistry>,
}

impl SessionManager {
    pub fn new(journal_dir: PathBuf, factory: Arc<dyn TransportFactory>) -> Result<Self> {
        let projects = Arc::new(ProjectRegistry::load(&journal_dir)?);
        Ok(Self {
            journal_dir,
            sessions: RwLock::new(HashMap::new()),
            factory,
            projects,
        })
    }

    /// Open a new session. The engine is spawned and registered before this
    /// returns; observers can `attach` immediately afterwards.
    ///
    /// Returns `(engine, Some(project))` when the spawn auto-created a new
    /// project for `cfg.cwd`, otherwise `(engine, None)`.
    pub async fn spawn(
        &self,
        mut cfg: SessionSpawnConfig,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Result<(Arc<SessionEngine>, Option<Project>)> {
        // Resolve (or create) the project for this cwd, then stamp the id into
        // cfg before the engine writes its metadata.
        let (project_id, created) = self.projects.resolve_or_create(&cfg.cwd)?;
        cfg.project_id = project_id.clone();

        let transport =
            self.factory
                .build(&cfg.agent, cfg.model.as_deref(), cfg.permission.as_deref())?;
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine = SessionEngine::spawn(transport, opts, cfg).await?;
        let id = engine.id().to_string();
        self.projects.register_session(&project_id, &id);
        self.sessions.write().await.insert(id, Arc::clone(&engine));
        Ok((engine, created))
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
        let project_id = self.projects.ensure_project(&meta.project_id, &meta.cwd)?;
        let cfg = SessionSpawnConfig {
            agent: meta.agent,
            cwd: meta.cwd,
            project_id: project_id.clone(),
            model: meta.model,
            permission: meta.permission,
            resume_cursor: meta.resume_cursor,
            tags: meta.tags,
        };
        let transport =
            self.factory
                .build(&cfg.agent, cfg.model.as_deref(), cfg.permission.as_deref())?;
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine = SessionEngine::resume(transport, opts, session_id.to_string(), cfg).await?;
        let id = engine.id().to_string();
        self.projects.register_session(&project_id, &id);
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

    /// Read a slice of a session's journal regardless of liveness: prefer the
    /// live engine's in-memory window (cheaper, monotonic), fall back to the
    /// on-disk archive if no live engine is registered. Single source of truth
    /// for poll-style readers — both the CLI and MCP tools route through here.
    pub async fn read_journal(&self, session_id: &str, from_seq: Seq) -> Result<Vec<JournalEntry>> {
        if let Some(engine) = self.get(session_id).await {
            return engine.snapshot(from_seq).await;
        }
        let archive = self.open_archive(session_id).await?;
        archive.replay_from(from_seq).await
    }

    /// Open the journal for a session that is not (currently) live. Errors if
    /// either the session IS live (use `get` instead) or no journal file
    /// exists.
    pub async fn open_archive(&self, session_id: &str) -> Result<crate::journal::ArchivedJournal> {
        if self.sessions.read().await.contains_key(session_id) {
            return Err(RoyError::Protocol(format!(
                "session {session_id} is live — use `get` to attach, not `open_archive`"
            )));
        }
        crate::journal::ArchivedJournal::open(&self.journal_dir, session_id).await
    }

    /// Resurrect every archived session in this manager's journal_dir at
    /// once. Returns a vector of `(session_id, error)` for entries that
    /// failed to resume (e.g. agent binary missing); the rest are now live.
    /// Used by `Daemon::run_with_opts` when `--resume-all` is set.
    pub async fn resume_all(
        &self,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Vec<(String, Option<RoyError>)> {
        let ids = match self.list_archived().await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "resume_all: failed to list archives; nothing resumed");
                return Vec::new();
            }
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.resume(&id, broadcast_capacity, mem_capacity).await {
                Ok(_) => out.push((id, None)),
                Err(e) => out.push((id, Some(e))),
            }
        }
        out
    }

    /// Close every session whose `last_activity` is older than `threshold`.
    /// Returns the closed session ids.
    pub async fn sweep_idle(&self, threshold: std::time::Duration) -> Vec<String> {
        let now = std::time::Instant::now();
        let to_close: Vec<String> = {
            let sessions = self.sessions.read().await;
            sessions
                .iter()
                .filter_map(|(id, engine)| {
                    if now.duration_since(engine.last_activity()) >= threshold {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        let mut closed = Vec::with_capacity(to_close.len());
        for id in to_close {
            if self.close(&id).await.is_ok() {
                closed.push(id);
            }
        }
        closed
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
        tracing::info!(session = %id, "closing session");
        engine.close()
    }

    /// Permanently remove an archived session's journal + metadata from disk.
    /// Refuses if the session is still live — caller must `close` first. The
    /// metadata file may not exist (e.g. tests that never persisted one), so
    /// its absence is not an error.
    pub async fn delete_archive(&self, id: &str) -> Result<()> {
        if self.sessions.read().await.contains_key(id) {
            return Err(RoyError::Protocol(format!(
                "session {id} is live — close it before deleting"
            )));
        }
        let jsonl = self.journal_dir.join(format!("{id}.jsonl"));
        let meta = self.journal_dir.join(format!("{id}.meta.json"));
        tokio::fs::remove_file(&jsonl).await.map_err(RoyError::Io)?;
        match tokio::fs::remove_file(&meta).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(RoyError::Io(e)),
        }
        if let Some(pid) = self.projects.project_of(id) {
            self.projects.unregister_session(&pid, id);
        }
        tracing::info!(session = %id, "deleted archived session");
        Ok(())
    }

    /// Scan journal_dir for *.meta.json files and populate the registry's
    /// session-index. Idempotent. Called once after construction to rebuild
    /// in-memory mapping from on-disk metadata.
    pub async fn index_existing_sessions(&self) -> Result<()> {
        if !tokio::fs::try_exists(&self.journal_dir)
            .await
            .map_err(RoyError::Io)?
        {
            return Ok(());
        }
        let mut entries = tokio::fs::read_dir(&self.journal_dir)
            .await
            .map_err(RoyError::Io)?;
        while let Some(entry) = entries.next_entry().await.map_err(RoyError::Io)? {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(sid) = name.strip_suffix(".meta.json") else {
                continue;
            };
            let meta = match read_metadata(&self.journal_dir, sid).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(session = %sid, error = %e, "skip indexing: meta unreadable");
                    continue;
                }
            };
            let pid = self.projects.ensure_project(&meta.project_id, &meta.cwd)?;
            self.projects.register_session(&pid, sid);
        }
        Ok(())
    }

    pub fn journal_dir(&self) -> &PathBuf {
        &self.journal_dir
    }

    pub fn factory(&self) -> &Arc<dyn TransportFactory> {
        &self.factory
    }

    pub fn projects(&self) -> &Arc<ProjectRegistry> {
        &self.projects
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
                env_remove: Vec::new(),
            })))
        }
    }

    static TMPDIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("roy-manager-test-{}-{n}", std::process::id()))
    }

    #[tokio::test]
    async fn resume_all_brings_back_closed_sessions() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(dir.clone(), Arc::new(FakeFactory)).expect("registry load");

        // Spawn → close two sessions to populate journals + metadata.
        let cfg = |suffix: &str| SessionSpawnConfig {
            agent: format!("agent-{suffix}"),
            cwd: std::env::current_dir().unwrap(),
            project_id: "test-project".into(),
            model: None,
            permission: None,
            resume_cursor: None,
            tags: std::collections::BTreeMap::default(),
        };
        let (e1, _) = mgr.spawn(cfg("a"), 256, 1024).await.unwrap();
        let (e2, _) = mgr.spawn(cfg("b"), 256, 1024).await.unwrap();
        let id1 = e1.id().to_string();
        let id2 = e2.id().to_string();
        mgr.close(&id1).await.unwrap();
        mgr.close(&id2).await.unwrap();
        // Brief settle: close is fire-and-forget; the engine actor drops the
        // input lease and shuts the handle down. We're not racing on that.
        assert!(mgr.list().await.is_empty());

        // Both ids should now be archived. resume_all brings them back.
        let mut archived = mgr.list_archived().await.unwrap();
        archived.sort();
        let mut expected = vec![id1.clone(), id2.clone()];
        expected.sort();
        assert_eq!(archived, expected);

        let results = mgr.resume_all(256, 1024).await;
        assert_eq!(results.len(), 2);
        for (_, err) in &results {
            assert!(err.is_none(), "resume_all failure: {err:?}");
        }
        let mut live = mgr.list().await;
        live.sort();
        assert_eq!(live, expected);

        for id in &expected {
            mgr.close(id).await.unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_idle_closes_quiet_sessions() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(dir.clone(), Arc::new(FakeFactory)).expect("registry load");

        let cfg = SessionSpawnConfig {
            agent: "opencode".into(),
            cwd: std::env::current_dir().unwrap(),
            project_id: "test-project".into(),
            model: None,
            permission: None,
            resume_cursor: None,
            tags: std::collections::BTreeMap::default(),
        };
        let (engine, _) = mgr.spawn(cfg, 256, 1024).await.unwrap();
        let id = engine.id().to_string();
        assert_eq!(mgr.list().await, vec![id.clone()]);

        // Below threshold → nothing closed.
        let closed = mgr.sweep_idle(std::time::Duration::from_secs(60)).await;
        assert!(closed.is_empty());
        assert_eq!(mgr.list().await, vec![id.clone()]);

        // Wait past a small threshold → session is swept.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let closed = mgr.sweep_idle(std::time::Duration::from_millis(100)).await;
        assert_eq!(closed, vec![id.clone()]);
        assert!(mgr.list().await.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn registry_lifecycle() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(dir.clone(), Arc::new(FakeFactory)).expect("registry load");
        assert!(mgr.list().await.is_empty());

        let cfg = SessionSpawnConfig {
            agent: "opencode".into(),
            cwd: std::env::current_dir().unwrap(),
            project_id: "test-project".into(),
            model: None,
            permission: None,
            resume_cursor: None,
            tags: std::collections::BTreeMap::default(),
        };
        let (engine, _) = mgr.spawn(cfg, 256, 1024).await.unwrap();
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

    #[tokio::test]
    async fn index_existing_sessions_rebuilds_project_membership() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let factory: Arc<dyn TransportFactory> = Arc::new(FakeFactory);
        let mgr = SessionManager::new(dir.clone(), factory).expect("registry load");

        // Hand-write a meta file referencing a project id that doesn't exist
        // in the registry; ensure_project must mint one for the meta's cwd.
        let session_id = "manual-sid";
        let proj_dir = dir.join("p1");
        std::fs::create_dir_all(&proj_dir).unwrap();
        let meta = crate::session_meta::SessionMetadata {
            session_id: session_id.into(),
            agent: "fake".into(),
            cwd: proj_dir.clone(),
            project_id: "pre-existing-uuid".into(),
            model: None,
            permission: None,
            resume_cursor: None,
            tags: Default::default(),
        };
        crate::session_meta::write_metadata(&dir, &meta).await.unwrap();
        // Write an empty journal file so it doesn't fail later scans.
        std::fs::write(dir.join(format!("{session_id}.jsonl")), "").unwrap();

        mgr.index_existing_sessions().await.unwrap();
        let projects = mgr.projects().list();
        assert_eq!(projects.len(), 1, "ensure_project should mint one for the meta's cwd");
        let sids = mgr.projects().sessions_in(&projects[0].id);
        assert_eq!(sids, vec![session_id.to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn spawn_auto_creates_project_for_new_cwd() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let factory: Arc<dyn TransportFactory> = Arc::new(FakeFactory);
        let mgr = SessionManager::new(dir.clone(), factory).expect("registry load");

        let proj_dir = dir.join("a");
        std::fs::create_dir_all(&proj_dir).unwrap();

        let cfg = SessionSpawnConfig {
            agent: "fake".into(),
            cwd: proj_dir.clone(),
            project_id: String::new(),
            model: None,
            permission: None,
            resume_cursor: None,
            tags: Default::default(),
        };
        let (engine, created) = mgr.spawn(cfg, 16, 16).await.unwrap();
        assert!(created.is_some(), "fresh cwd must auto-create project");
        let pid = created.unwrap().id;
        assert_eq!(mgr.projects().sessions_in(&pid), vec![engine.id().to_string()]);

        // Second spawn into same cwd reuses the same project.
        let cfg2 = SessionSpawnConfig {
            agent: "fake".into(),
            cwd: proj_dir.clone(),
            project_id: String::new(),
            model: None,
            permission: None,
            resume_cursor: None,
            tags: Default::default(),
        };
        let (_engine2, created2) = mgr.spawn(cfg2, 16, 16).await.unwrap();
        assert!(created2.is_none(), "existing project must not be re-created");
        let mut sids = mgr.projects().sessions_in(&pid);
        sids.sort();
        assert_eq!(sids.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
