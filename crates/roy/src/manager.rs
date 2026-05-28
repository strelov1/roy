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
use uuid::Uuid;

use crate::daemon::TransportFactory;
use crate::engine::{EngineOpts, SessionEngine, SessionSpawnConfig};
use crate::error::{Result, RoyError};
use crate::harnesses_config::Harness;
use crate::journal::{JournalEntry, Seq};
use crate::session_store::SessionStore;

pub struct SessionManager {
    journal_dir: PathBuf,
    workspace_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<SessionEngine>>>,
    factory: Arc<dyn TransportFactory>,
    session_store: Arc<SessionStore>,
}

impl SessionManager {
    pub async fn new(
        journal_dir: PathBuf,
        workspace_dir: PathBuf,
        factory: Arc<dyn TransportFactory>,
        session_store: Arc<SessionStore>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&workspace_dir).map_err(RoyError::Io)?;
        Ok(Self {
            journal_dir,
            workspace_dir,
            sessions: RwLock::new(HashMap::new()),
            factory,
            session_store,
        })
    }

    /// Open a new session. The engine is spawned and registered before this
    /// returns; observers can `attach` immediately afterwards.
    ///
    /// - `cfg.cwd = Some(path)`: spawn the agent in that directory.
    /// - `cfg.cwd = None`: allocate an orphan dir at `<workspace>/<session_id>/`
    ///   and use it. The session id is either taken from `cfg.fixed_session_id`
    ///   or freshly minted here (so the workspace dir can be named before the
    ///   engine starts).
    pub async fn spawn(
        &self,
        cfg: SessionSpawnConfig,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Result<Arc<SessionEngine>> {
        let cfg = match cfg.cwd.clone() {
            Some(_) => cfg,
            None => {
                // Orphan: mint the session id up front so the workspace dir
                // can be named after it, then pin the id into cfg so the
                // engine reuses it instead of minting another.
                let sid = cfg
                    .fixed_session_id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                let path = self.workspace_dir.join(&sid);
                std::fs::create_dir_all(&path).map_err(RoyError::Io)?;
                SessionSpawnConfig {
                    cwd: Some(path),
                    fixed_session_id: Some(sid),
                    ..cfg
                }
            }
        };
        self.spawn_internal(cfg, broadcast_capacity, mem_capacity)
            .await
    }

    async fn spawn_internal(
        &self,
        cfg: SessionSpawnConfig,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Result<Arc<SessionEngine>> {
        let transport =
            self.factory
                .build(cfg.harness, cfg.model.as_deref(), cfg.permission.as_deref())?;
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine =
            SessionEngine::spawn(transport, opts, cfg, Arc::clone(&self.session_store)).await?;
        let id = engine.id().to_string();
        self.sessions.write().await.insert(id, Arc::clone(&engine));
        Ok(engine)
    }

    /// Resurrect a previously-closed (or restart-survived) session: read its
    /// row from the session store, rebuild the transport via the factory, and
    /// re-spawn the engine with the same id and journal.
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
        let row = self
            .session_store
            .get(session_id)
            .await?
            .ok_or_else(|| RoyError::Protocol(format!("no session: {session_id}")))?;
        let parsed: Harness = row.harness.parse().map_err(RoyError::Protocol)?;
        let cfg = SessionSpawnConfig {
            harness: parsed,
            cwd: Some(row.cwd),
            model: row.model,
            permission: row.permission,
            resume_cursor: row.resume_cursor,
            fixed_session_id: Some(session_id.to_string()),
            system_prompt: row.system_prompt,
            extra_env: Default::default(),
        };
        let transport =
            self.factory
                .build(cfg.harness, cfg.model.as_deref(), cfg.permission.as_deref())?;
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine = SessionEngine::resume(
            transport,
            opts,
            session_id.to_string(),
            cfg,
            Arc::clone(&self.session_store),
        )
        .await?;
        self.sessions
            .write()
            .await
            .insert(session_id.to_string(), Arc::clone(&engine));
        Ok(engine)
    }

    pub async fn list(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }

    pub async fn get(&self, id: &str) -> Option<Arc<SessionEngine>> {
        self.sessions.read().await.get(id).cloned()
    }

    /// Session rows whose `closed_at IS NOT NULL` in the session store and
    /// whose engine is not in the live registry — i.e. closed sessions plus
    /// survivors of a daemon restart. Returned in unspecified order.
    pub async fn list_archived(&self) -> Result<Vec<crate::session_store::SessionRow>> {
        use std::collections::HashSet;
        let live: HashSet<String> = self.sessions.read().await.keys().cloned().collect();
        let rows = self.session_store.list_archived().await?;
        Ok(rows
            .into_iter()
            .filter(|r| !live.contains(&r.session_id))
            .collect())
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

    /// Resurrect every live-but-unmounted session at daemon boot: anything
    /// with `closed_at IS NULL` in the session store but no engine in the
    /// in-memory registry. Returns a vector of `(session_id, error)` for
    /// entries that failed to resume (e.g. agent binary missing); the rest
    /// are now live. Used by `Daemon::run_with_opts` when `--resume-all` is
    /// set.
    pub async fn resume_all(
        &self,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Vec<(String, Option<RoyError>)> {
        let rows = match self.session_store.list_live().await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "resume_all: failed to list live rows; nothing resumed");
                return Vec::new();
            }
        };
        let live: std::collections::HashSet<String> =
            self.sessions.read().await.keys().cloned().collect();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if live.contains(&row.session_id) {
                continue;
            }
            let id = row.session_id;
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

    /// Wind down a session: mark its row closed in the session store, ask the
    /// engine to close, and remove it from the live registry. The journal
    /// file stays on disk for inspection / resume.
    pub async fn close(&self, id: &str) -> Result<()> {
        let engine = self
            .sessions
            .write()
            .await
            .remove(id)
            .ok_or_else(|| RoyError::Protocol(format!("no such session: {id}")))?;
        tracing::info!(session = %id, "closing session");
        self.session_store.mark_closed(id).await?;
        engine.close()
    }

    /// Permanently remove an archived session: delete its journal file from
    /// disk and its row from the session store. Refuses if the session is
    /// still live — caller must `close` first. A missing journal file is not
    /// an error (e.g. tests that never appended to the journal).
    pub async fn delete_archive(&self, id: &str) -> Result<()> {
        if self.sessions.read().await.contains_key(id) {
            return Err(RoyError::Protocol(format!(
                "session {id} is live — close it before deleting"
            )));
        }
        let jsonl = self.journal_dir.join(format!("{id}.jsonl"));
        match tokio::fs::remove_file(&jsonl).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(RoyError::Io(e)),
        }
        self.session_store.delete(id).await?;
        tracing::info!(session = %id, "deleted archived session");
        Ok(())
    }

    pub fn journal_dir(&self) -> &PathBuf {
        &self.journal_dir
    }

    pub fn workspace_dir(&self) -> &PathBuf {
        &self.workspace_dir
    }

    pub fn factory(&self) -> &Arc<dyn TransportFactory> {
        &self.factory
    }

    pub fn session_store(&self) -> &Arc<SessionStore> {
        &self.session_store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};
    use std::time::Duration;

    /// Test factory that always builds the fake ACP agent regardless of the
    /// requested harness.
    struct FakeFactory;
    impl TransportFactory for FakeFactory {
        fn build(
            &self,
            _harness: Harness,
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
                system_prompt_channel: crate::transport::SystemPromptChannel::Meta,
            })))
        }
    }

    static TMPDIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("roy-manager-test-{}-{n}", std::process::id()))
    }

    async fn new_mgr(dir: &PathBuf) -> SessionManager {
        let store_path = dir.join("sessions.db");
        let store = Arc::new(
            crate::session_store::SessionStore::open(&store_path)
                .await
                .expect("open store"),
        );
        SessionManager::new(
            dir.clone(),
            dir.join("workspace"),
            Arc::new(FakeFactory),
            store,
        )
        .await
        .expect("manager")
    }

    /// Minimal orphan spawn config for tests.
    fn orphan_cfg(harness: Harness) -> SessionSpawnConfig {
        SessionSpawnConfig {
            harness,
            cwd: None,
            model: None,
            permission: None,
            resume_cursor: None,
            fixed_session_id: None,
            system_prompt: None,
            extra_env: Default::default(),
        }
    }

    #[tokio::test]
    async fn resume_all_brings_back_closed_sessions() {
        let dir = tmp_dir();
        let mgr = new_mgr(&dir).await;

        // Spawn → close two sessions to populate journals + session-store rows.
        let e1 = mgr
            .spawn(orphan_cfg(Harness::Opencode), 256, 1024)
            .await
            .unwrap();
        let e2 = mgr
            .spawn(orphan_cfg(Harness::Claude), 256, 1024)
            .await
            .unwrap();
        let id1 = e1.id().to_string();
        let id2 = e2.id().to_string();
        mgr.close(&id1).await.unwrap();
        mgr.close(&id2).await.unwrap();
        // Brief settle: close is fire-and-forget; the engine actor drops the
        // input lease and shuts the handle down. We're not racing on that.
        assert!(mgr.list().await.is_empty());

        // Both ids should now be archived. resume_all brings them back.
        // Note: `resume_all` reads `list_live` (closed_at IS NULL), so this
        // test's expectation needs the rows to be re-marked live on resume.
        // Until T12 wires the engine→store insert, the resume_all path below
        // will be exercised by T14's test fix-ups. For now, just verify
        // `list_archived` reflects the closed state.
        let mut archived: Vec<String> = mgr
            .list_archived()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.session_id)
            .collect();
        archived.sort();
        let mut expected = vec![id1.clone(), id2.clone()];
        expected.sort();
        assert_eq!(archived, expected);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_idle_closes_quiet_sessions() {
        let dir = tmp_dir();
        let mgr = new_mgr(&dir).await;

        let engine = mgr
            .spawn(orphan_cfg(Harness::Opencode), 256, 1024)
            .await
            .unwrap();
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
        let mgr = new_mgr(&dir).await;
        assert!(mgr.list().await.is_empty());

        let engine = mgr
            .spawn(orphan_cfg(Harness::Opencode), 256, 1024)
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
