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
    /// returns; observers can `attach` immediately afterwards.
    pub async fn spawn(
        &self,
        transport: Arc<dyn Transport>,
        cwd: PathBuf,
        broadcast_capacity: usize,
        mem_capacity: usize,
    ) -> Result<Arc<SessionEngine>> {
        let opts = EngineOpts {
            journal_dir: self.journal_dir.clone(),
            broadcast_capacity,
            mem_capacity,
        };
        let engine = SessionEngine::spawn(transport, cwd, opts).await?;
        let id = engine.id().to_string();
        self.sessions
            .write()
            .await
            .insert(id, Arc::clone(&engine));
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
    use crate::event::TurnEvent;
    use crate::provider::Provider;
    use crate::transport::PrintTransport;

    struct FakeProvider;
    impl Provider for FakeProvider {
        fn command(&self) -> &str {
            "tests/scripts/fake-agent.sh"
        }
        fn spawn_args(&self, _: &str, _: Option<&str>) -> Vec<String> {
            vec![]
        }
        fn encode_user_message(&self, t: &str) -> String {
            format!("{t}\n")
        }
        fn parse_line(&self, line: &str) -> Option<TurnEvent> {
            crate::provider::ClaudeProvider::new(None).parse_line(line)
        }
        fn is_turn_end(&self, ev: &TurnEvent) -> bool {
            matches!(ev, TurnEvent::Result { .. })
        }
    }

    fn tmp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "roy-manager-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[tokio::test]
    async fn registry_lifecycle() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(dir.clone());
        assert!(mgr.list().await.is_empty());

        let transport: Arc<dyn Transport> = Arc::new(PrintTransport::new(Arc::new(FakeProvider)));
        let engine = mgr
            .spawn(transport, std::env::current_dir().unwrap(), 256, 1024)
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
