//! Project — a working-directory grouping of sessions. Persisted as a single
//! `~/.roy/projects.json` registry file plus a `project_id` field on every
//! `SessionMetadata`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A user-visible project — one canonical filesystem path with a display name
/// and a stable UUID id. Sessions are owned by exactly one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: u64,
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
}
