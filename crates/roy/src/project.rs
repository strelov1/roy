//! Project — a working-directory grouping of sessions. Persisted as a single
//! `~/.roy/projects.json` registry file plus a `project_id` field on every
//! `SessionMetadata`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, RoyError};

/// A user-visible project — one canonical filesystem path with a display name
/// and a stable UUID id. Sessions are owned by exactly one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: u64,
}

/// Canonicalise a project path: resolve symlinks, make absolute, strip
/// Windows UNC prefix. Single gate for any path entering the registry —
/// keeps equivalent paths from minting duplicate projects.
pub fn canonicalize_for_project(p: &Path) -> Result<PathBuf> {
    let abs = std::fs::canonicalize(p).map_err(RoyError::Io)?;
    Ok(dunce::simplified(&abs).to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_serde_roundtrip() {
        let p = Project {
            id: "1f7c-uuid".to_string(),
            name: "claude-agent".to_string(),
            path: PathBuf::from("/Users/i_strelov/Projects/claude-agent"),
            created_at: 1722345600,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn canonicalize_resolves_existing_path() {
        let cwd = std::env::current_dir().unwrap();
        let canonical = canonicalize_for_project(&cwd).unwrap();
        assert!(canonical.is_absolute());
    }

    #[test]
    fn canonicalize_errors_on_missing_path() {
        let bogus = std::env::temp_dir().join("definitely-does-not-exist-roy-test");
        let _ = std::fs::remove_dir_all(&bogus);
        let err = canonicalize_for_project(&bogus).unwrap_err();
        assert!(matches!(err, RoyError::Io(_)));
    }
}
