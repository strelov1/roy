//! Resolve the absolute filesystem cwd of a session and validate that the
//! resulting path stays inside the workspace root. Only mkdir is performed —
//! no auto-generated CLAUDE.md or .memory/ files.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum CwdError {
    #[error("invalid id (must be UUID-shape)")]
    InvalidId,
    #[error("path escape")]
    Escape,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub enum CwdScope {
    Personal,
    Team,
}

pub struct CwdInput {
    pub scope: CwdScope,
    pub user_id: String,
    pub team_id: Option<String>,
    pub project_id: Option<String>,
    pub session_id: String,
}

fn is_uuid_shape(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn resolve_cwd(workspace_dir: &Path, input: CwdInput) -> Result<PathBuf, CwdError> {
    if !is_uuid_shape(&input.user_id) {
        return Err(CwdError::InvalidId);
    }
    if !is_uuid_shape(&input.session_id) {
        return Err(CwdError::InvalidId);
    }
    if let Some(ref t) = input.team_id {
        if !is_uuid_shape(t) {
            return Err(CwdError::InvalidId);
        }
    }
    if let Some(ref p) = input.project_id {
        if !is_uuid_shape(p) {
            return Err(CwdError::InvalidId);
        }
    }
    let root = match input.scope {
        CwdScope::Personal => workspace_dir.join("users").join(&input.user_id),
        CwdScope::Team => match &input.team_id {
            Some(t) => workspace_dir.join("teams").join(t),
            None => return Err(CwdError::InvalidId),
        },
    };
    let path = match &input.project_id {
        Some(p) => root
            .join("projects")
            .join(p)
            .join("sessions")
            .join(&input.session_id),
        None => root.join("sessions").join(&input.session_id),
    };
    require_safe_path(workspace_dir, &path)?;
    Ok(path)
}

fn require_safe_path(workspace_dir: &Path, p: &Path) -> Result<(), CwdError> {
    let workspace = workspace_dir.canonicalize()?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
        let canonical = parent.canonicalize()?;
        if !canonical.starts_with(&workspace) {
            return Err(CwdError::Escape);
        }
    }
    Ok(())
}
