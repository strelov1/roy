# Project entity — design

> **v2 (workspace-model) — current**. The original draft assumed `project.path`
> was a user-chosen filesystem directory. That model was replaced before
> implementation; the current code (branch `feature/projects`) implements the
> workspace-model described below. The original v1 design is preserved at the
> bottom of this document under "Appendix: v1 (superseded)".

Status: implemented on `feature/projects`.
Date: 2026-05-23.
Author: brainstorm with `strelov1`.

## Goal

Add a first-class **Project** entity to roy so sessions can be grouped by
a named working directory in the web UI. A project is the unit of
organisation visible in the sidebar; every session either belongs to one
project or is an **orphan**.

## Non-goals

- Per-project defaults (default agent / model / permission) — out of scope.
- Project icons / colours / descriptions — out of scope.
- Moving a session between projects — out of scope.
- Renaming a project — out of scope in v2. Name is immutable.
- External `rm -rf` detection beyond surfacing the next stat failure.

## Decisions (v2)

| # | Decision |
|---|---|
| 1 | A **workspace** directory owns all project and orphan-session subdirectories. Default: `~/.roy/workspace/`; override via `ROY_WORKSPACE` or `roy serve --workspace-dir`. |
| 2 | Project = `{ id: UUID, name: String, path: PathBuf, created_at: u64 }`. `path = workspace_dir/name`. Name is the directory key; immutable after creation. |
| 3 | Name validated against `^[A-Za-z0-9_-]+$`. |
| 4 | A session belongs to exactly one project (cwd = `project.path`) OR is an **orphan** (cwd = `workspace_dir/<session_id>/`). |
| 5 | `SessionMetadata.project_id: Option<String>` — `None` means orphan. |
| 6 | `Spawn { project_id: Option<String>, … }` — no `cwd` field. `None` → orphan; directory created at spawn. |
| 7 | `CreateProject { name }` — no `path` argument. Daemon derives `path = workspace_dir/name`. |
| 8 | Delete project = cascade: removes registry entry + every session's `.jsonl`/`.meta.json`. Does **not** remove `<workspace>/<name>/` on disk. |
| 9 | Persistence: `<journal_dir>/projects.json` plus `project_id: Option<String>` on `SessionMetadata`. |
| 10 | No `RenameProject`. No auto-resolve on unknown `project_id` at startup — orphaned meta files are logged as warnings. |

## Architecture

`ProjectRegistry` is owned by `SessionManager`:

```
Daemon
└── SessionManager
    ├── engines: HashMap<sid, SessionEngine>
    ├── journal_dir: PathBuf
    ├── workspace_dir: PathBuf                     (new)
    └── projects: ProjectRegistry                  (new)
        ├── file_path: PathBuf    (<journal_dir>/projects.json)
        └── inner: Mutex<RegistryState>
            ├── projects: Vec<Project>
            └── sessions_by_project: HashMap<project_id, BTreeSet<sid>>
```

`sessions_by_project` is rebuilt at startup by scanning `.meta.json` files.
If a meta references an unknown `project_id`, the session is logged as a
warning and skipped — no auto-create, user must clean up by hand.

## Data model

### `Project` (`crates/roy/src/project.rs`)

```rust
pub struct Project {
    pub id: String,          // UUID v4
    pub name: String,        // ^[A-Za-z0-9_-]+$; immutable
    pub path: PathBuf,       // workspace_dir/name
    pub created_at: u64,     // unix seconds
}
```

### Registry file `<journal_dir>/projects.json`

```json
{
  "version": 1,
  "projects": [
    { "id": "1f7c…", "name": "roy", "path": "/Users/alice/.roy/workspace/roy", "created_at": 1722345600 }
  ]
}
```

Atomic write: temp file in same directory + `rename`.
Missing `version` → `v1`. Unknown `version` → hard error.

### `SessionMetadata` (changed field)

| field | type | meaning |
|-------|------|---------|
| `project_id` | `Option<String>` | UUID of owning project; `null` = orphan |

`cwd` stays on `SessionMetadata` so files remain self-contained.

## Wire protocol

### `ClientCommand`s

```rust
Spawn { agent, project_id: Option<String>, model, permission, resume }
CreateProject { name: String }
DeleteProject { project_id: String }
ListProjects
```

`Spawn.project_id = None` → orphan session; daemon creates
`workspace_dir/<session_id>/` and sets that as `cwd`.

`CreateProject` creates `workspace_dir/<name>/` on disk and adds the
registry entry.

### `ServerEvent`s (new and changed)

```rust
Spawned { session: String, project_id: Option<String>, resume_cursor: Option<String> }
ProjectsListed { projects: Vec<Project> }
ProjectCreated { project: Project }
ProjectDeleted { project_id: String, deleted_sessions: Vec<String> }
```

`SessionInfo.project_id: Option<String>` — included in `listed` and
`listed_archived` payloads.

### New `ErrorCode`s

`no_project`, `project_exists`, `create_project_failed`,
`delete_project_failed`, `invalid_project_name`.

## CLI

```bash
roy projects list
roy projects create <name>
roy projects delete <project_id|name> [--yes]
roy run --project <name>          # project session
roy run                            # orphan session
roy serve --workspace-dir <path>
```

## MCP

Three new tools:

| Tool | Behaviour |
|---|---|
| `roy_list_projects` | wraps `ListProjects` |
| `roy_create_project` | `name` only |
| `roy_delete_project` | `project_id`; returns deleted session ids |

`roy_run`, `roy_run_detached`, `roy_fire` accept `project_id` (no `cwd`).

## Spawn flow

1. Client sends `Spawn { project_id: Some("abc…"), … }`.
2. Daemon looks up project by id; errors with `no_project` if not found.
3. `cwd = project.path`. Engine spawns agent there.
4. `Spawned { session, project_id: Some("abc…"), … }` returned.

Orphan path: `project_id = None` → `cwd = workspace_dir/<session_id>/`
(mkdir at spawn). `Spawned.project_id = None`.

## Cascade delete

Phase 1 (under Mutex): snapshot session ids, remove project entry from
`projects`, remove from `sessions_by_project`, persist registry.

Phase 2 (outside Mutex): for each session id — close live engine, delete
`.jsonl` and `.meta.json`. Failures are `warn!`-logged; `ProjectDeleted`
still fires.

On-disk `<workspace>/<name>/` directory is **not** removed — the user may
have committed work in there. Documented in release notes.

## UI

```
Projects ⌄
  ⊞ New project
  ▸ claude-agent
  ▾ roy
    · Переписка с fernando     ← active
    · Проверка ссылки…
```

- "New project" modal: `name` field only (path derived automatically).
- Project kebab → Delete (no Rename in v2).
- "New chat" defaults to last-used project; `None` → orphan.
- Expansion state in `localStorage["roy:expanded_projects"]`.

## Errors

| Situation | Behaviour |
|---|---|
| `CreateProject { name }` with invalid name | `Error { code: invalid_project_name }` |
| `CreateProject { name }` already exists | `Error { code: project_exists }` |
| `CreateProject` mkdir fails | `Error { code: create_project_failed }` |
| `Spawn { project_id: Some(_) }` unknown id | `Error { code: no_project }` |
| `DeleteProject` unknown id | `Error { code: no_project }` |
| Cascade close/erase fails for a session | `warn!`-logged; others proceed |
| `projects.json` corrupt at startup | log error, init empty registry, back up as `projects.json.bak` |
| Meta references unknown `project_id` | warning + skip; not added to index |

## Testing

### Unit

- `CreateProject` with invalid name → error.
- `CreateProject` twice same name → `project_exists`.
- `Spawn { project_id: None }` → orphan directory created.
- `DeleteProject` cascade: meta + journal files gone; workspace dir stays.
- Restart: `ProjectRegistry` rebuilt from `projects.json`; sessions re-indexed.
- `projects.json` with unknown `version` → hard error.

### Integration (`crates/roy/tests/projects.rs`)

- `CreateProject` + `ListProjects` round-trip.
- `Spawn` with project → `Spawned.project_id` set; `cwd` = project path.
- `Spawn` without project → `Spawned.project_id` null; orphan dir created.
- `DeleteProject` cascade — sessions gone; `ListProjects` empty.
- Restart-and-resume: manager rebuilt from disk; project and sessions intact.
- One test over WebSocket framing.

## Rollout

1. Wipe `~/.roy/journals/` (manual; documented in release notes).
2. Set `ROY_WORKSPACE` or `--workspace-dir` if non-default path desired.
3. Deploy daemon + `roy-web` together. No staged migration.

---

## Appendix: v1 (superseded)

Preserved for context; superseded by the v2 model above.

The original v1 design assumed that:

- A project's `path` was a **user-chosen absolute filesystem directory**
  (e.g. `/Users/alice/Projects/claude-agent`), not a subdirectory of a
  managed workspace.
- `CreateProject { path: PathBuf, name: Option<String> }` — name defaulted
  to `basename(canonical_path)`.
- `Spawn { cwd: PathBuf, … }` — a spawn would auto-call `resolve_or_create`
  against the cwd, creating a project automatically on first use.
- `project_id: String` was **required** on `SessionMetadata`; old meta files
  without it would error. The journal dir was to be wiped before deploy.
- `RenameProject { project_id, name }` and `ProjectRenamed` were included.
- `Spawned` carried `project: Option<Project>` — `Some` when auto-created.
- Path was canonicalised and deduplicated so two equivalent paths couldn't
  spawn two projects.

Why v1 was replaced: the workspace-model (v2) removes the dependency on
the user's existing directory tree, makes `CreateProject` a pure name
operation, and gives the daemon full control of every project's on-disk
location. It also eliminates the ambiguous auto-create-on-spawn behaviour
and the awkward "required project_id" constraint on old meta files.
