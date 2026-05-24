//! Persistent `chat_id → roy session_id` map, backed by one JSON file.
//!
//! Writes are serialized through an in-memory `tokio::sync::Mutex`; each
//! mutation rewrites the whole file (atomic via `tempfile` + rename).
//! That is fine at chat-bot scale (low write rate, few hundred entries
//! at most). If volume ever justifies it, swap in sled later — the
//! `SessionBinder` API surface is the migration boundary.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// chat_id → roy session_id
    bindings: HashMap<i64, String>,
}

#[derive(Debug)]
pub struct SessionBinder {
    path: PathBuf,
    state: Mutex<State>,
}

impl SessionBinder {
    /// Load existing bindings, or initialize empty if the file does not exist.
    pub async fn load(path: PathBuf) -> Result<Self> {
        let state = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("reading {}", path.display())));
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub async fn get(&self, chat_id: i64) -> Option<String> {
        self.state.lock().await.bindings.get(&chat_id).cloned()
    }

    pub async fn set(&self, chat_id: i64, session_id: String) -> Result<()> {
        let mut guard = self.state.lock().await;
        guard.bindings.insert(chat_id, session_id);
        Self::persist(&self.path, &*guard).await
    }

    pub async fn forget(&self, chat_id: i64) -> Result<()> {
        let mut guard = self.state.lock().await;
        guard.bindings.remove(&chat_id);
        Self::persist(&self.path, &*guard).await
    }

    async fn persist(path: &std::path::Path, state: &State) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(state).context("serializing binder")?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("creating binder dir {}", parent.display()))?;
            }
        }
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, &bytes)
            .await
            .with_context(|| format!("writing {}", tmp.display()))?;
        tokio::fs::rename(&tmp, path)
            .await
            .with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let binder = SessionBinder::load(dir.path().join("missing.json"))
            .await
            .unwrap();
        assert!(binder.get(42).await.is_none());
    }

    #[tokio::test]
    async fn set_then_get() {
        let dir = tempfile::tempdir().unwrap();
        let binder = SessionBinder::load(dir.path().join("b.json"))
            .await
            .unwrap();
        binder.set(7, "sess-1".into()).await.unwrap();
        assert_eq!(binder.get(7).await.as_deref(), Some("sess-1"));
    }

    #[tokio::test]
    async fn forget_removes() {
        let dir = tempfile::tempdir().unwrap();
        let binder = SessionBinder::load(dir.path().join("b.json"))
            .await
            .unwrap();
        binder.set(7, "sess-1".into()).await.unwrap();
        binder.forget(7).await.unwrap();
        assert!(binder.get(7).await.is_none());
    }
}
