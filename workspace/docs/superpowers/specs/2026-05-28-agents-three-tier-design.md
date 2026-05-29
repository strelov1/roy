# Three-tier agents: built-in, user, team

Status: approved (2026-05-28)
Builds on: `docs/superpowers/specs/2026-05-27-agents-as-files-design.md` (file-based agents v1).
Spans: roy (Rust), roy-web (Svelte), roy-docker.

## Problem

The v1 file-based design has one flat directory: `~/.roy/agents/`. Anyone with access to the daemon sees and edits the same set. That doesn't scale to:
- Multiple users sharing a daemon — each should have their own personal agents.
- Team collaboration — agents shared across team members.
- A baseline set of `roy-*` presets shipped with the product, common to everyone.

## Storage layout

```
/home/roy/.roy/agents/                                  ← built-in, read-only
/home/roy/.roy/workspace/users/<user_id>/.roy/agents/   ← personal, user RW
/home/roy/.roy/workspace/teams/<team_id>/.roy/agents/   ← team, members RW
```

The user/team paths follow the existing CWD scheme from `crates/roy-management/src/cwd.rs:51-65`. Today that file builds session CWDs as `workspace/users/<uid>/sessions/<sid>/`; the new agents dir sits as a sibling at `workspace/users/<uid>/.roy/agents/`.

Built-ins live in HOME root (`/home/roy/.roy/agents/`) — outside the workspace tree. Read-only for end users; updated only by image rebuild.

File format is unchanged from v1 (YAML frontmatter `name` / `description` / `engine` / optional `model`, body = system prompt).

## Backend changes (roy-management)

### Source resolution

`crates/roy-management/src/agents.rs::list_agents_from(&Path)` becomes:

```rust
pub struct AgentSource {
    pub scope: AgentScope,
    pub dir: PathBuf,
}

pub enum AgentScope {
    Builtin,
    Personal,           // user_id is in the dir path; not duplicated in struct
    Team { id: String },
}

pub async fn list_all_agents(
    builtin_dir: &Path,                  // /home/roy/.roy/agents
    workspace_dir: &Path,                // /home/roy/.roy/workspace
    user_id: &str,                       // from JWT
    team_ids: &[String],                 // from roy-auth memberships
) -> Vec<AgentFile> { ... }
```

The function builds source paths:
- `builtin_dir`
- `workspace_dir/users/<user_id>/.roy/agents`
- `workspace_dir/teams/<team_id>/.roy/agents` for each team

Calls `list_dir(path, scope)` on each, merges, sorts by name within scope.

### `AgentFile` shape

```rust
#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    pub name: String,
    pub description: String,
    pub engine: String,
    pub model: Option<String>,
    pub body: String,

    // NEW: which scope this agent came from.
    pub scope: AgentScopeWire,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AgentScopeWire {
    Builtin,
    Personal,
    Team { team_id: String },
}
```

### HTTP handler

`GET /management/agents` becomes user-aware. The existing `require_user` middleware already injects `AuthUser`. Handler reads `user.id` and queries team memberships via roy-auth.

```rust
async fn list_agent_files(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<AgentFile>>, ApiError> {
    let team_ids = roy_auth::teams_for(&s.auth_pool, &user.id).await?;
    Ok(Json(crate::agents::list_all_agents(
        Path::new("/home/roy/.roy/agents"),  // BUILTIN_DIR, configurable via env
        &s.workspace_dir,
        &user.id,
        &team_ids,
    ).await))
}
```

The 30-second cache (`AgentsCache`) keys on `(user_id, team_ids)` — different users get different cached results. Eviction on `invalidate()` clears the whole cache (simpler than per-user eviction; the next get re-scans).

### Env vars for the daemon

When `roy-daemon` spawns an ACP child for a chat session, it sets these env vars on the child process so the `roy-agent-builder` skill can resolve the right write path:

- `ROY_AGENTS_DIR_USER=/home/roy/.roy/workspace/users/<user_id>/.roy/agents` — always present.
- `ROY_AGENTS_DIR_TEAM_<team_slug>=/home/roy/.roy/workspace/teams/<team_id>/.roy/agents` — one per team the user belongs to. Team-slug uses the team's display name lowercased + kebab-case'd (collision-free because team names are unique per user in roy-auth).
- `ROY_TEAMS=<team_slug>,<team_slug>,...` — comma-separated list so the skill can present choices.

The daemon already has the spawn machinery for `ROY_SESSION_ID` (see `roy/src/daemon.rs` around the ACP spawn). Add the new vars in the same code path. They need user identity and team memberships, which the daemon has when handling a `Spawn` command (or can fetch via roy-auth, since the daemon already owns the auth DB pool).

## Permissions

Built-in dir (`/home/roy/.roy/agents/`) inside the container: owned by `roy:roy`, mode 0755. Files mode 0644. RO is enforced by **convention** (we just don't tell the skill to write there) — Linux perms aren't restrictive because we trust the daemon process not to misuse them. Operators editing built-ins do so by rebuilding the image.

User dir (`workspace/users/<uid>/.roy/agents`): created on first write by the chat session. Owned by `roy:roy` (the container user). Mode 0755 dir, 0644 files. Backed by the `roy-home` named volume — never bind-mounted from the host, so no uid-mapping issues.

Team dir: same as user dir, under `workspace/teams/<tid>/`.

Remove the bind mount `${HOME}/.roy/agents:/home/roy/.roy/agents` that v1 introduced — built-ins now live in the image, and personal/team dirs live in the workspace volume.

## Built-in delivery

New directory `roy-docker/builtin-agents/` in the roy-docker tree (or roy repo — pick during impl). Contains hand-curated `roy-*.md` files:

- `roy-coder.md` — generic coding helper.
- `roy-reviewer.md` — code review assistant.
- ... extensible.

`Dockerfile.roy` adds:

```dockerfile
COPY roy-docker/builtin-agents/ /home/roy/.roy/agents/
RUN chown -R roy:roy /home/roy/.roy/agents
```

Place this AFTER the `useradd roy` + `mkdir -p /home/roy/.roy/workspace` block and BEFORE `USER roy`.

The existing v1 bind-mount line for `${HOME}/.roy/agents:/home/roy/.roy/agents` is **removed** from `roy-docker/docker-compose.yml` (both `roy-daemon` and `roy-management` services). Host changes to `~/.roy/agents` no longer affect the container.

## Frontend changes (roy-web)

### Agent type

```ts
// src/lib/agents.svelte.ts
export type AgentScope =
  | { kind: 'builtin' }
  | { kind: 'personal' }
  | { kind: 'team'; team_id: string };

export type Agent = {
  name: string;
  description: string;
  engine: AgentPreset;
  model?: string;
  body: string;
  scope: AgentScope;
};
```

The wire shape matches the new `AgentScopeWire` Rust enum (`{ kind: 'builtin' }` / `{ kind: 'personal' }` / `{ kind: 'team', team_id }`). Defensive parser drops entries with unknown `kind`.

### Visual treatment

`AgentsView.svelte` cards gain a small scope chip alongside the engine chip:

```
[engine:codex]  [scope:roy]
[engine:claude] [scope:personal]
[engine:claude] [scope:team · GTM]
```

Scope label resolution:
- `builtin` → `roy` (lowercase, monospace).
- `personal` → `personal`.
- `team` → look up team name from `authState.user.teams` by `team_id`, fall back to `team · <id-prefix>`.

No filter UI in v1 — all three scopes mix in one grid. Sort: builtin first, then personal, then team (matches the natural priority of "always there" → "mine" → "shared"). Within each scope sort by name.

ModelPicker Agents tab uses the same `agentsStore` — no separate UI changes there (tab shows whatever the store returns).

## Skill changes (`roy-agent-builder`)

The skill is updated to ask the user which scope to save into and uses the daemon-exposed env vars to resolve the path. No user_id lookup, no team_id lookup — env vars carry that.

### Interview flow

After collecting purpose / engine / model / slug:

1. Read `ROY_TEAMS` env var. If non-empty, present choices: "personal" or one of the team slugs.
2. If only personal exists: skip — save under `$ROY_AGENTS_DIR_USER/<slug>.md`.
3. Otherwise: ask "Save as personal or to team <X>?" — wait for answer.
4. Resolve target:
   - personal → `$ROY_AGENTS_DIR_USER`
   - team `<slug>` → `$ROY_AGENTS_DIR_TEAM_<slug>`
5. Confirm full path, then Write.

### Path resolution in tools

The LLM uses Bash to expand env vars before calling Write (the harness Write tool doesn't expand shell vars). Pattern:

```bash
echo "$ROY_AGENTS_DIR_USER/<slug>.md"
```

Use the printed absolute path with Write.

### Builtins out of scope

The skill never writes to `/home/roy/.roy/agents/` — that's operator territory. If the user asks "edit roy-coder", the skill refuses with: "Built-in agents are part of the daemon image. Edit `roy-docker/builtin-agents/roy-coder.md` and rebuild."

## Failure modes

- `ROY_AGENTS_DIR_USER` not set: daemon needs an update — flag explicitly so the user knows to rebuild.
- User has no team memberships: `ROY_TEAMS` is empty or unset; skill silently picks personal.
- Slug collides across scopes: each card shows its scope chip; the picker (later) needs a `(scope, name)` composite key. For v1 the list shows duplicates with different chips; the user disambiguates visually.
- A team is deleted while a session is live: env vars frozen at spawn time; the skill might write to a stale team dir. Acceptable — restart session to refresh.

## Migration

- Existing files under `~/.roy/agents/*.md` (v1 bind mount) on existing hosts:
  - After rebuild, that path is no longer mounted into the container.
  - Files remain on host but invisible to the daemon.
  - User can manually move them into the container's volume via `docker cp` if they want.
  - Recommended: clean break — recreate the agents in the new layout via the skill.

## Open questions

None. Implementation details (env var naming conflicts, exact daemon code locations) handed off to the plan.
