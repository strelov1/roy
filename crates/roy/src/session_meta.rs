//! Per-session metadata persisted alongside the journal as
//! `<journal_dir>/<session_id>.meta.json`. Lets the daemon resurrect a live
//! session after a restart: read the metadata to know which agent/cwd/etc.
//! to spawn, then hand the stored `resume_cursor` to the transport so the
//! underlying ACP `session/load` reconnects to the agent-side session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, RoyError};

/// Static + cursor fields for a session, kept in sync with the journal on
/// disk. `resume_cursor` is mutated as the agent reports a new cursor; the
/// rest are set at spawn and never change for the session's lifetime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: String,
    pub agent: String,
    pub cwd: PathBuf,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
}

/// Compute the metadata file path for `session_id` in `dir`.
pub fn meta_path(dir: &Path, session_id: &str) -> PathBuf {
    dir.join(format!("{session_id}.meta.json"))
}

/// Atomically write metadata to `<dir>/<session_id>.meta.json` (temp + rename).
pub async fn write_metadata(dir: &Path, meta: &SessionMetadata) -> Result<()> {
    tokio::fs::create_dir_all(dir).await.map_err(RoyError::Io)?;
    let final_path = meta_path(dir, &meta.session_id);
    let tmp_path = final_path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(meta).map_err(|e| RoyError::Protocol(e.to_string()))?;
    tokio::fs::write(&tmp_path, &json)
        .await
        .map_err(RoyError::Io)?;
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .map_err(RoyError::Io)?;
    Ok(())
}

/// Read metadata from `<dir>/<session_id>.meta.json`. Errors if missing.
pub async fn read_metadata(dir: &Path, session_id: &str) -> Result<SessionMetadata> {
    let path = meta_path(dir, session_id);
    let bytes = tokio::fs::read(&path).await.map_err(RoyError::Io)?;
    serde_json::from_slice(&bytes).map_err(|e| RoyError::Protocol(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmpdir() -> PathBuf {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("roy-session-meta-test-{}-{n}", std::process::id()))
    }

    #[tokio::test]
    async fn write_and_read_roundtrip() {
        let dir = tmpdir();
        let meta = SessionMetadata {
            session_id: "sid-1".to_string(),
            agent: "opencode".to_string(),
            cwd: PathBuf::from("/tmp/foo"),
            project_id: "test-project".to_string(),
            model: None,
            permission: Some("allow".to_string()),
            resume_cursor: Some("acp-sid-x".to_string()),
            tags: BTreeMap::from([("foo".to_string(), "bar".to_string())]),
        };
        write_metadata(&dir, &meta).await.unwrap();
        let back = read_metadata(&dir, "sid-1").await.unwrap();
        assert_eq!(meta, back);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_errors_when_missing() {
        let dir = tmpdir();
        let _ = std::fs::create_dir_all(&dir);
        assert!(read_metadata(&dir, "missing").await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
