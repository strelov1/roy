//! Project — a named, workspace-managed directory grouping sessions. Each
//! project lives at `<workspace_dir>/<name>/`; the name is immutable after
//! creation (it IS the directory key). Persisted as a single
//! `~/.roy/projects.json` registry file plus a `project_id` field on every
//! `SessionMetadata`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Result, RoyError};

/// A user-visible project. `path` is derived from `workspace_dir + name` at
/// creation time and stored for wire-protocol stability. Because `name` IS the
/// on-disk directory key, **renaming is not supported** — create a new project
/// instead. Sessions can be orphan (no project) in which case the daemon
/// allocates `<workspace_dir>/<session_id>/` for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: u64,
}

/// Validate a project name: `^[A-Za-z0-9_-]+$`, non-empty, no hidden names
/// starting with `.`. Returns `Err` with a human-readable message on failure.
pub fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(RoyError::InvalidProjectName {
            name: name.to_string(),
            reason: "name must not be empty".to_string(),
        });
    }
    if name.starts_with('.') {
        return Err(RoyError::InvalidProjectName {
            name: name.to_string(),
            reason: "name must not start with '.'".to_string(),
        });
    }
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-' {
            return Err(RoyError::InvalidProjectName {
                name: name.to_string(),
                reason: format!(
                    "name may only contain ASCII letters, digits, '_', '-'; got '{ch}'"
                ),
            });
        }
    }
    Ok(())
}

/// On-disk shape of `~/.roy/projects.json`. `version` is the schema version;
/// unknown versions error rather than silently degrading.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    projects: Vec<Project>,
}

fn default_version() -> u32 {
    1
}
const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Default)]
struct RegistryState {
    projects: Vec<Project>,
    /// Derived index: not serialised, rebuilt at init from meta files.
    sessions_by_project: HashMap<String, BTreeSet<String>>,
}

/// Persistent registry of projects. Mutex-guarded value, **never** held across
/// `.await`. All IO is sync (write file in a single shot) and happens under
/// the lock; that is acceptable because the file is tiny (one JSON object
/// for the whole project list).
#[derive(Debug)]
pub struct ProjectRegistry {
    file_path: PathBuf,
    workspace_dir: PathBuf,
    inner: Mutex<RegistryState>,
}

impl ProjectRegistry {
    /// Path of the registry file inside `journal_dir`.
    pub fn file_path_for(journal_dir: &Path) -> PathBuf {
        journal_dir.join("projects.json")
    }

    /// Load (or initialise empty) the registry. If the file is unreadable or
    /// has an unknown `version`, returns an error so callers can decide
    /// whether to back it up.
    ///
    /// `workspace_dir` is stored in the instance for path synthesis; it is
    /// NOT serialised into `projects.json` — it comes from the daemon config.
    pub fn load(journal_dir: &Path, workspace_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(journal_dir).map_err(RoyError::Io)?;
        let file_path = Self::file_path_for(journal_dir);
        let projects = if file_path.exists() {
            let bytes = std::fs::read(&file_path).map_err(RoyError::Io)?;
            let parsed: RegistryFile = serde_json::from_slice(&bytes)
                .map_err(|e| RoyError::Protocol(format!("projects.json: {e}")))?;
            if parsed.version != CURRENT_VERSION {
                return Err(RoyError::Protocol(format!(
                    "projects.json: unsupported version {}",
                    parsed.version
                )));
            }
            parsed.projects
        } else {
            Vec::new()
        };
        Ok(Self {
            file_path,
            workspace_dir,
            inner: Mutex::new(RegistryState {
                projects,
                sessions_by_project: HashMap::new(),
            }),
        })
    }

    /// Sync write: temp + rename, identical pattern to session_meta.
    fn persist(&self, state: &RegistryState) -> Result<()> {
        let on_disk = RegistryFile {
            version: CURRENT_VERSION,
            projects: state.projects.clone(),
        };
        let bytes =
            serde_json::to_vec_pretty(&on_disk).map_err(|e| RoyError::Protocol(e.to_string()))?;
        let tmp = self.file_path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(RoyError::Io)?;
        std::fs::rename(&tmp, &self.file_path).map_err(RoyError::Io)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<Project> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .projects
            .clone()
    }

    /// Create a new project named `name` inside the workspace. Validates the
    /// name, creates `<workspace_dir>/<name>/`, and pushes the record.
    /// Errors if a project with that name already exists.
    pub fn create_project(&self, name: &str) -> Result<Project> {
        validate_project_name(name)?;
        let path = self.workspace_dir.join(name);
        let mut state = self.inner.lock().expect("registry poisoned");
        if state.projects.iter().any(|p| p.name == name) {
            return Err(RoyError::ProjectExists {
                name: name.to_string(),
            });
        }
        std::fs::create_dir_all(&path).map_err(RoyError::Io)?;
        let project = Project {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            path,
            created_at: unix_now(),
        };
        state.projects.push(project.clone());
        self.persist(&state)?;
        Ok(project)
    }

    /// Verify a project id is in the registry. Returns `Ok(project_id)` if
    /// registered, else an error. Sessions whose meta refers to a missing
    /// project — fail to resume with a loud error.
    pub fn ensure_project(&self, project_id: &str) -> Result<String> {
        let state = self.inner.lock().expect("registry poisoned");
        if state.projects.iter().any(|p| p.id == project_id) {
            return Ok(project_id.to_string());
        }
        Err(RoyError::Protocol(format!(
            "project not found: {project_id}; registry may be corrupt or the project was deleted"
        )))
    }

    /// Get the path for a project by id.
    pub fn project_path(&self, project_id: &str) -> Result<PathBuf> {
        let state = self.inner.lock().expect("registry poisoned");
        state
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.path.clone())
            .ok_or_else(|| RoyError::Protocol(format!("no project: {project_id}")))
    }

    /// Allocate `<workspace_dir>/<session_id>/` for an orphan session (one
    /// spawned with no project). Creates the directory and returns its path.
    pub fn allocate_orphan_session_dir(&self, session_id: &str) -> Result<PathBuf> {
        let path = self.workspace_dir.join(session_id);
        std::fs::create_dir_all(&path).map_err(RoyError::Io)?;
        Ok(path)
    }

    /// Remove the project entry from the in-memory state and persist, and
    /// return the set of session ids that were attached to it (so the caller
    /// can cascade-close them outside the lock). Errors if id unknown.
    pub fn remove_entry(&self, id: &str) -> Result<Vec<String>> {
        let mut state = self.inner.lock().expect("registry poisoned");
        let pos = state
            .projects
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| RoyError::Protocol(format!("no_project: {id}")))?;
        state.projects.remove(pos);
        let sids = state
            .sessions_by_project
            .remove(id)
            .unwrap_or_default()
            .into_iter()
            .collect();
        self.persist(&state)?;
        Ok(sids)
    }

    /// Register a session under a project. Idempotent.
    pub fn register_session(&self, project_id: &str, session_id: &str) {
        let mut state = self.inner.lock().expect("registry poisoned");
        state
            .sessions_by_project
            .entry(project_id.to_string())
            .or_default()
            .insert(session_id.to_string());
    }

    /// Unregister a session. Idempotent.
    pub fn unregister_session(&self, project_id: &str, session_id: &str) {
        let mut state = self.inner.lock().expect("registry poisoned");
        if let Some(set) = state.sessions_by_project.get_mut(project_id) {
            set.remove(session_id);
        }
    }

    /// Snapshot of session ids attached to a project.
    pub fn sessions_in(&self, project_id: &str) -> Vec<String> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .sessions_by_project
            .get(project_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Look up the project id (if any) for a session.
    pub fn project_of(&self, session_id: &str) -> Option<String> {
        let state = self.inner.lock().expect("registry poisoned");
        state.sessions_by_project.iter().find_map(|(pid, sids)| {
            if sids.contains(session_id) {
                Some(pid.clone())
            } else {
                None
            }
        })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_serde_roundtrip() {
        let p = Project {
            id: "1f7c-uuid".to_string(),
            name: "my-project".to_string(),
            path: PathBuf::from("/home/user/.roy/workspace/my-project"),
            created_at: 1722345600,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn validate_project_name_accepts_valid_names() {
        for name in ["hello", "my-project", "proj_1", "ABC123", "a"] {
            validate_project_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn validate_project_name_rejects_invalid_names() {
        assert!(validate_project_name("").is_err(), "empty name must fail");
        assert!(
            validate_project_name(".hidden").is_err(),
            "hidden name must fail"
        );
        assert!(
            validate_project_name("has/slash").is_err(),
            "slash must fail"
        );
        assert!(
            validate_project_name("has space").is_err(),
            "space must fail"
        );
        assert!(validate_project_name("has.dot").is_err(), "dot must fail");
    }

    fn tmp_dirs() -> (PathBuf, PathBuf) {
        static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("roy-proj-test-{}-{n}", std::process::id()));
        let journal = base.join("journals");
        let workspace = base.join("workspace");
        let _ = std::fs::remove_dir_all(&base);
        (journal, workspace)
    }

    #[test]
    fn load_initialises_empty_when_no_file() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace).unwrap();
        assert!(reg.list().is_empty());
    }

    #[test]
    fn persist_then_load_roundtrip() {
        let (journal, workspace) = tmp_dirs();
        let ws2 = workspace.clone();
        let reg = ProjectRegistry::load(&journal, workspace).unwrap();
        {
            let mut state = reg.inner.lock().unwrap();
            state.projects.push(Project {
                id: "abc".into(),
                name: "demo".into(),
                path: PathBuf::from("/tmp/demo"),
                created_at: 42,
            });
            reg.persist(&state).unwrap();
        }
        let reg2 = ProjectRegistry::load(&journal, ws2).unwrap();
        let list = reg2.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "abc");
    }

    #[test]
    fn load_errors_on_unknown_version() {
        let (journal, workspace) = tmp_dirs();
        std::fs::create_dir_all(&journal).unwrap();
        std::fs::write(
            ProjectRegistry::file_path_for(&journal),
            br#"{"version":99,"projects":[]}"#,
        )
        .unwrap();
        let err = ProjectRegistry::load(&journal, workspace).unwrap_err();
        assert!(matches!(err, RoyError::Protocol(_)));
    }

    #[test]
    fn create_project_creates_dir_and_persists() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace.clone()).unwrap();
        let p = reg.create_project("my-proj").unwrap();
        assert_eq!(p.name, "my-proj");
        assert_eq!(p.path, workspace.join("my-proj"));
        assert!(p.path.is_dir(), "project dir must be created");

        // Re-load verifies persistence.
        let reg2 = ProjectRegistry::load(&journal, workspace.clone()).unwrap();
        assert_eq!(reg2.list().len(), 1);
        assert_eq!(reg2.list()[0].name, "my-proj");
    }

    #[test]
    fn create_project_rejects_duplicate_name() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace).unwrap();
        reg.create_project("dup").unwrap();
        let err = reg.create_project("dup").unwrap_err();
        assert!(matches!(err, RoyError::ProjectExists { .. }));
    }

    #[test]
    fn create_project_rejects_invalid_name() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace).unwrap();
        assert!(reg.create_project("has/slash").is_err());
        assert!(reg.create_project(".hidden").is_err());
        assert!(reg.create_project("").is_err());
    }

    #[test]
    fn allocate_orphan_session_dir_creates_dir() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace.clone()).unwrap();
        let sid = "orphan-session-id";
        let path = reg.allocate_orphan_session_dir(sid).unwrap();
        assert_eq!(path, workspace.join(sid));
        assert!(path.is_dir(), "orphan dir must be created");
    }

    #[test]
    fn ensure_project_returns_id_when_present() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace).unwrap();
        let p = reg.create_project("ensure-test").unwrap();
        let id = reg.ensure_project(&p.id).unwrap();
        assert_eq!(id, p.id);
    }

    #[test]
    fn ensure_project_errors_when_absent() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace).unwrap();
        assert!(reg.ensure_project("does-not-exist").is_err());
    }

    #[test]
    fn remove_entry_returns_session_ids_and_drops_project() {
        let (journal, workspace) = tmp_dirs();
        let reg = ProjectRegistry::load(&journal, workspace).unwrap();
        let p = reg.create_project("del-test").unwrap();
        reg.register_session(&p.id, "s1");
        reg.register_session(&p.id, "s2");
        let mut sids = reg.remove_entry(&p.id).unwrap();
        sids.sort();
        assert_eq!(sids, vec!["s1".to_string(), "s2".to_string()]);
        assert!(reg.list().is_empty());
    }
}
