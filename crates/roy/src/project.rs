//! Project — a working-directory grouping of sessions. Persisted as a single
//! `~/.roy/projects.json` registry file plus a `project_id` field on every
//! `SessionMetadata`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    pub fn load(journal_dir: &Path) -> Result<Self> {
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
        let bytes = serde_json::to_vec_pretty(&on_disk)
            .map_err(|e| RoyError::Protocol(e.to_string()))?;
        let tmp = self.file_path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(RoyError::Io)?;
        std::fs::rename(&tmp, &self.file_path).map_err(RoyError::Io)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<Project> {
        self.inner.lock().expect("registry poisoned").projects.clone()
    }

    /// Look up the project for `cwd` or create a new one if absent. Returns
    /// `(project_id, Some(project))` when freshly created, otherwise
    /// `(project_id, None)`. Canonicalises `cwd` first.
    pub fn resolve_or_create(&self, cwd: &Path) -> Result<(String, Option<Project>)> {
        let canonical = canonicalize_for_project(cwd)?;
        let mut state = self.inner.lock().expect("registry poisoned");
        if let Some(p) = state.projects.iter().find(|p| p.path == canonical) {
            return Ok((p.id.clone(), None));
        }
        let project = Project {
            id: uuid::Uuid::new_v4().to_string(),
            name: basename_or_path(&canonical),
            path: canonical,
            created_at: unix_now(),
        };
        let id = project.id.clone();
        state.projects.push(project.clone());
        self.persist(&state)?;
        Ok((id, Some(project)))
    }
}

fn basename_or_path(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
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

    fn tmp_journal_dir() -> PathBuf {
        static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!(
            "roy-proj-test-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn load_initialises_empty_when_no_file() {
        let dir = tmp_journal_dir();
        let reg = ProjectRegistry::load(&dir).unwrap();
        assert!(reg.list().is_empty());
    }

    #[test]
    fn persist_then_load_roundtrip() {
        let dir = tmp_journal_dir();
        let reg = ProjectRegistry::load(&dir).unwrap();
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
        let reg2 = ProjectRegistry::load(&dir).unwrap();
        let list = reg2.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "abc");
    }

    #[test]
    fn load_errors_on_unknown_version() {
        let dir = tmp_journal_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            ProjectRegistry::file_path_for(&dir),
            br#"{"version":99,"projects":[]}"#,
        )
        .unwrap();
        let err = ProjectRegistry::load(&dir).unwrap_err();
        assert!(matches!(err, RoyError::Protocol(_)));
    }

    #[test]
    fn resolve_or_create_creates_then_resolves() {
        let dir = tmp_journal_dir();
        let reg = ProjectRegistry::load(&dir).unwrap();
        let project_dir = dir.join("proj-a");
        std::fs::create_dir_all(&project_dir).unwrap();

        let (id1, created1) = reg.resolve_or_create(&project_dir).unwrap();
        assert!(created1.is_some(), "first call must create");
        let (id2, created2) = reg.resolve_or_create(&project_dir).unwrap();
        assert!(created2.is_none(), "second call must reuse");
        assert_eq!(id1, id2);
    }

    #[test]
    fn resolve_or_create_is_concurrency_safe() {
        use std::sync::Arc;
        let dir = tmp_journal_dir();
        let reg = Arc::new(ProjectRegistry::load(&dir).unwrap());
        let project_dir = dir.join("proj-conc");
        std::fs::create_dir_all(&project_dir).unwrap();

        let mut handles = Vec::new();
        for _ in 0..32 {
            let reg = Arc::clone(&reg);
            let p = project_dir.clone();
            handles.push(std::thread::spawn(move || {
                reg.resolve_or_create(&p).unwrap().0
            }));
        }
        let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let first = ids[0].clone();
        assert!(ids.iter().all(|i| i == &first), "all threads must agree on id");
        assert_eq!(reg.list().len(), 1, "only one project must exist");
    }
}
