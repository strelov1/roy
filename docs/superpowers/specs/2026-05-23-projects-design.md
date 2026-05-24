# Project entity — design

Status: draft, awaiting user review.
Date: 2026-05-23.
Author: brainstorm with `strelov1`.

## Goal

Add a first-class **Project** entity to roy so sessions can be grouped by
working directory in the web UI. A project is the unit of organisation
visible in the sidebar; every session is owned by exactly one project.

Concrete trigger: the user wants to be able to point at
`/Users/i_strelov/Projects/claude-agent` (and other directories) as
top-level entities in the sidebar and have all sessions in that
directory live underneath.

## Non-goals

- Per-project defaults (default agent / model / permission) — out of
  scope, may follow.
- Project icons / colours / descriptions — out of scope.
- Moving a session between projects — out of scope. A session's
  `project_id` is fixed at spawn.
- Renaming a project's `path` — out of scope. Renaming `name` is in.
- Detecting external `rm -rf` of a project directory beyond surfacing
  the next stat failure to the UI.

## Decisions (resolved during brainstorm)

| # | Decision |
|---|---|
| 1 | Project = directory. `id` is a UUID; `path` and `name` are attributes. |
| 2 | A session belongs to exactly one project; `session.cwd == project.path` is an invariant. |
| 3 | Existing sessions are wiped manually (`rm ~/.roy/journals/*`); no migration code. |
| 4 | Project fields: minimum — `{id, name, path, created_at}`. No defaults yet. |
| 5 | CLI/MCP: `Spawn { cwd }` triggers `resolve_or_create` in the registry — no API breakage, auto-create on first use. |
| 6 | Delete project = cascade (close + erase journals of all its sessions) with explicit user confirmation in UI. |
| 7 | Persistence: single `~/.roy/projects.json` plus a new `project_id` field on every `SessionMetadata`. No per-project directory layout. |

## Architecture

`ProjectRegistry` is owned by `SessionManager` (not a separate actor):

```
Daemon
└── SessionManager
    ├── engines: HashMap<sid, SessionEngine>
    ├── journal_dir: PathBuf
    └── projects: ProjectRegistry         (new)
        ├── file_path: PathBuf            (~/.roy/projects.json)
        └── inner: std::sync::Mutex<RegistryState>   // not held across .await
            ├── projects: Vec<Project>
            └── sessions_by_project: HashMap<project_id, BTreeSet<session_id>>  (derived)
```

Rationale: project ops are infrequent file-backed mutations with no
long-lived resources. A Mutex-guarded value is the right shape; an
actor would add ceremony with no upside.

`sessions_by_project` is rebuilt at daemon start by scanning meta
files (same scan that already drives `list_archived` / `resume_all`).

## Data model

### `Project` (`crates/roy/src/project.rs`, new)

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,        // UUID v4
    pub name: String,
    pub path: PathBuf,     // absolute, canonicalised, symlinks resolved
    pub created_at: u64,   // unix seconds
}
```

### Registry file `~/.roy/projects.json`

```json
{
  "version": 1,
  "projects": [
    {
      "id": "1f7c…",
      "name": "claude-agent",
      "path": "/Users/i_strelov/Projects/claude-agent",
      "created_at": 1722345600
    }
  ]
}
```

Atomic write via tmp + rename, identical pattern to
`session_meta::write_metadata`. Missing `version` field is read as
`v1`. Unknown `version` is a hard error (no silent ignore).

### `SessionMetadata` change

```rust
pub struct SessionMetadata {
    pub session_id: String,
    pub agent: String,
    pub cwd: PathBuf,
    pub project_id: String,   // NEW — required, no default
    // …unchanged: model, permission, resume_cursor, tags
}
```

`project_id` is **required**; deserialising an old meta file without
it errors out. This is intentional — we already agreed to wipe the
existing journal directory before deploying the change.

`cwd` is kept on `SessionMetadata` even though it duplicates
`project.path`. Trade-off: meta files stay self-contained (no
registry lookup needed to interpret one). On a system with tens of
sessions the duplication is negligible.

### Canonicalisation

A single helper handles every path that enters the registry:

```rust
fn canonicalize_for_project(p: &Path) -> Result<PathBuf> {
    let abs = std::fs::canonicalize(p).map_err(RoyError::Io)?;
    Ok(dunce::simplified(&abs).to_path_buf())
}
```

- Symlinks are resolved.
- Non-existing path → `Io` error (we do not create directories on the
  user's behalf).
- Single gate prevents two equivalent paths from spawning two
  projects.

## Wire protocol changes

### New `ClientCommand`s

```rust
ListProjects,
CreateProject { path: PathBuf, name: Option<String> },
RenameProject { project_id: String, name: String },
DeleteProject { project_id: String },
```

- `CreateProject.name == None` → daemon uses `basename(canonical_path)`.
- `DeleteProject` is cascade and synchronous.

### New `ServerEvent`s

```rust
ProjectsListed { projects: Vec<Project> },
ProjectCreated { project: Project },
ProjectRenamed { project: Project },                              // full object
ProjectDeleted { project_id: String, deleted_sessions: Vec<String> },
```

### Changed events

```rust
Spawned {
    session: String,
    project_id: String,
    project: Option<Project>,        // Some when auto-created in this spawn
    resume_cursor: Option<String>,
}

pub struct SessionRef { pub id: String, pub project_id: String }

Listed { sessions: Vec<SessionRef> },
ListedArchived { sessions: Vec<SessionRef> },
```

`project_id` next to every session id lets the sidebar group in
constant time without per-session `read_journal` calls.

### New `ErrorCode`s

```
NoProject, ProjectExists, CreateProjectFailed, DeleteProjectFailed
```

### Why no Hello/version handshake

Wire schema is bumped without backward compatibility. The only
consumer is the web client in the sibling repo `roy-web`, deployed
in lockstep with the daemon. If outside consumers appear later, a
`Hello` event with `schema_version` becomes the migration vector.

## CLI

```bash
roy projects list
roy projects create <path> [--name NAME]
roy projects rename <project_id|name> <new_name>
roy projects delete <project_id|name> [--yes]
```

`roy run --cwd /path` keeps its signature; output now contains
`project_id` (and optionally a banner when a project was
auto-created).

## MCP

Three new tools in `crates/roy-cli/src/mcp.rs`:

| Tool | Behaviour |
|---|---|
| `roy_list_projects` | wraps `ListProjects` |
| `roy_create_project` | accepts `path`, optional `name` |
| `roy_delete_project` | accepts `project_id`, returns deleted session ids |

Existing `roy_run` / `roy_run_detached` are unchanged in their
parameters; their responses include `project_id`.

## Spawn flow

```rust
impl ProjectRegistry {
    pub fn resolve_or_create(&self, cwd: &Path)
        -> Result<(String, Option<Project>)>
    {
        let canonical = canonicalize_for_project(cwd)?;
        let mut state = self.inner.lock().expect("poisoned");
        if let Some(p) = state.projects.iter().find(|p| p.path == canonical) {
            return Ok((p.id.clone(), None));
        }
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: basename_or_path(&canonical),
            path: canonical,
            created_at: now_unix(),
        };
        let id = project.id.clone();
        state.projects.push(project.clone());
        self.persist(&state)?;
        Ok((id, Some(project)))
    }
}
```

- Single Mutex acquisition covers lookup + insert + persist; concurrent
  `Spawn { cwd: same }` deterministically resolves to one project.
- `Spawned.project: Some(p)` only when auto-created in this call.

## Cascade delete

```rust
pub async fn delete(&self, manager: &SessionManager, id: &str)
    -> Result<Vec<String>>
{
    // Phase 1 — under lock: snapshot session ids, remove project, persist.
    let session_ids = {
        let mut state = self.inner.lock().expect("poisoned");
        let pos = state.projects.iter().position(|p| p.id == id)
            .ok_or(/* no_project */)?;
        state.projects.remove(pos);
        let sids = state.sessions_by_project.remove(id).unwrap_or_default();
        self.persist(&state)?;
        sids.into_iter().collect::<Vec<_>>()
    };
    // Phase 2 — outside lock: close engines + erase journals.
    for sid in &session_ids {
        let _ = manager.close(sid).await;
        let _ = manager.delete_archive(sid).await;
    }
    Ok(session_ids)
}
```

Lock is **not** held across IO. Per-session FS failures are
`warn!`-logged; `ProjectDeleted` still fires — the user-visible state
is consistent.

Race window between project removal and journal removal: a new spawn
in the same cwd can auto-create a fresh project before the old
sessions' files are gone. That is acceptable — new project has a new
UUID, stale `.jsonl` files are unrelated and will be cleaned up
imminently.

## UI

### Sidebar layout

```
[logo]                       [toggle]
+ New chat

Projects ⌄
  ⊞ New project
  ▸ claude-agent
  ▾ roy
    · Переписка с fernando        ← active
    · Проверка ссылки…
    · Neia
  ▸ Mercado
  ▸ Investment

No archived sessions
```

### Behaviour

- Project row click toggles expansion. Expansion state persisted to
  `localStorage["roy:expanded_projects"]` keyed by `project_id`.
  Default: expanded when `currentSession` belongs to it, else
  collapsed.
- Session row click → existing `goto(\`/s/\${sid}\`)`.
- Project row kebab `⋯` → Rename / Delete.
- "New project" opens a modal with `path` (required) + `name`
  (defaults to `basename(path)`).
- "New chat" requires picking a project before spawning (dropdown
  with last-used pre-selected).

### Routes

- `/` — landing.
- `/s/<session_id>` — open session (unchanged).
- `/p/<project_id>` — out of scope for v1.

### Files

| File | Purpose |
|---|---|
| `Sidebar.svelte` | hosts the section, existing |
| `ProjectGroup.svelte` *(new)* | project row + nested session list |
| `ProjectRow.svelte` *(new)* | header row (caret + icon + name + kebab) |
| `NewProjectDialog.svelte` *(new)* | create modal |
| `DeleteProjectDialog.svelte` *(new)* | confirm with "N sessions will be deleted" |
| `SessionRow.svelte` | unchanged — reused as child |
| `ChatHeader.svelte` | adds `<project name> / <session title>` breadcrumb |

### State changes (`state.svelte.ts`)

- `projects = $state<Project[]>([])`.
- `live`/`archived` become `SessionRef[]` instead of `string[]`.
- `expandedProjects = $state<Record<string, boolean>>(loadFromStorage())`
  with `$effect` to persist.
- `sessionsByProject = $derived(...)` — derived index used by sidebar.
- New methods: `createProject`, `renameProject`, `deleteProject`.
- `refreshSessions` additionally calls `ListProjects`.
- On `ProjectDeleted.deleted_sessions`: clear `currentSession` if
  contained, prune `titles`, `activeSessions`, `bgSubs`.

## Errors and edge cases

| Situation | Behaviour |
|---|---|
| `CreateProject` with non-existing path | `Error { code: CreateProjectFailed }` |
| `CreateProject` with path already used | `Error { code: ProjectExists }`. UI suggests "Use existing '<name>'?" |
| `Spawn` without cwd, no `ROY_CWD`, falls back to `current_dir` | works; auto-creates project on `current_dir` with warn log |
| `DeleteProject` with unknown id | `Error { code: NoProject }` |
| Cascade delete fails on one session's close | warn-logged, others proceed, `ProjectDeleted` still fires |
| `projects.json` corrupt at startup | log error, init empty registry, rescan meta files to recover; back up bad file as `projects.json.bak` |
| Meta references `project_id` not in registry (post-corruption) | rebuild a proxy entry: `id` from meta, `path` from meta's `cwd`, `name = basename_or_path(cwd)`, `created_at = now()`. Persisted on next mutation. Never leave a session orphan. |
| Concurrent rename + delete same project | Mutex serialises; first wins |
| `roy projects delete <name>` with ambiguous name | error "specify id" |
| Directory removed externally | sidebar marks "⚠ missing" (icon only). Spawn into it errors; delete still works |

## Testing

### Unit — `crates/roy/src/project.rs`

- `canonicalize_for_project` resolves symlinks, errors on missing.
- `resolve_or_create` idempotent per cwd; distinguishes different cwds.
- 100 concurrent `resolve_or_create` with the same cwd → exactly one
  project.
- `delete` snapshots under lock and does not hold the lock across IO.
- `persist` is atomic — partial tmp without rename leaves prior file
  intact.
- `version: 2` registry → hard error.

### Integration — `crates/roy/tests/projects.rs` (new)

E2E via `tokio::io::duplex` (matches `daemon.rs` test style):

- `CreateProject` + `ListProjects` round-trip.
- `Spawn` with brand new cwd → `Spawned { project: Some(_) }`.
- Second `Spawn` into same project → `project: None`, same `project_id`.
- `DeleteProject` cascade — `Listed` no longer contains those
  sessions; their `.jsonl` files gone.
- `RenameProject` — `ListProjects` reflects new name.
- Restart-and-resume: spawn then re-init `SessionManager` from disk;
  `ListProjects` returns the same project; sessions still tied.
- One of the above also runs over the WebSocket framing.

### Regression on real-CLI smoke tests

`real_claude` / `real_gemini` / `real_opencode` / `real_codex`:
verify `Spawned.project_id` and auto-create flow against real
directories.

### Web

Manual smoke (no unit harness in `roy-web` yet):

1. Create project at `/Users/i_strelov/Projects/claude-agent`.
2. Spawn chat — verify project auto-expands with the new session.
3. Switch sessions across projects.
4. Rename project.
5. Delete project — confirm cascade.

## Rollout

1. Wipe `~/.roy/journals/` (manual; documented in `CHANGELOG` /
   release notes).
2. Deploy daemon + `roy-web` together. No staged migration.

## Open questions

(None at design time — flagged for the implementation plan if any
appear during build.)
