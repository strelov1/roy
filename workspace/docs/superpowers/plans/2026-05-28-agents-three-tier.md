# Three-tier agents implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split agents into three scopes — built-in (`/home/roy/.roy/agents/`), personal (`workspace/users/<uid>/.roy/agents/`), team (`workspace/teams/<tid>/.roy/agents/`) — so each user can keep private personas and share team-wide ones, while a baseline `roy-*` set ships with the daemon image.

**Architecture:** Backend learns to scan three sources, filtered by JWT identity, and tags each agent with its scope. Daemon's ACP-spawn path gets a new `extra_env` channel through the `Spawn` wire so management can hand the chat session a `ROY_AGENTS_DIR_USER` / `ROY_AGENTS_DIR_TEAM_<slug>` map. `roy-agent-builder` skill reads those env vars to decide where to write. Built-ins are COPY'd into the image; the v1 bind mount goes away.

**Tech Stack:**
- `/Users/i_strelov/Projects/roy` — Rust workspace. Tests via `cargo test`.
- `/Users/i_strelov/Projects/roy-web` — Svelte 5 + Vite + TS. `npm run check`.
- `/Users/i_strelov/Projects/roy-docker` — Dockerfile + compose. No tests.

**Spec:** `docs/superpowers/specs/2026-05-28-agents-three-tier-design.md`

---

## File map

### roy (Rust)

| Action | Path | Responsibility |
|---|---|---|
| Modify | `crates/roy/src/control.rs` | Add `extra_env: Option<HashMap<String,String>>` to `ClientCommand::Spawn` |
| Modify | `crates/roy/src/transport/mod.rs` (or wherever `Transport` trait lives) | `Transport::open` signature gains `extra_env: &HashMap<String, String>` |
| Modify | `crates/roy/src/transport/acp/mod.rs:200-211` | Apply each `extra_env` entry via `cmd.env(k, v)` after `ROY_SESSION_ID` |
| Modify | `crates/roy/src/manager.rs` (or daemon spawn entry) | Forward `extra_env` from `Spawn` to `Transport::open` |
| Modify | `crates/roy-management/src/agents.rs` | Three-source `list_all_agents`, scope-tagged `AgentFile` |
| Modify | `crates/roy-management/src/http.rs` | Handler reads `Extension<AuthUser>`, fetches teams, filters |
| Modify | `crates/roy-management/src/roy_client.rs` | `Spawn` wrapper forwards `extra_env` |
| Modify | `crates/roy-management/src/http.rs` (session create) | Compute env vars from user_id + teams before Spawn |
| Modify | `crates/roy-management/src/state.rs` | `AppState` gains `auth_pool` (if not present) for team lookups |

### roy-web (Svelte)

| Action | Path | Responsibility |
|---|---|---|
| Modify | `src/lib/agents.svelte.ts` | Add `AgentScope` union + `scope` field on `Agent` |
| Modify | `src/lib/AgentsView.svelte` | Render scope chip; sort builtin → personal → team |
| Modify | `src/lib/ModelPicker.svelte` | Pass-through — agentsStore shape changes are invisible to picker logic |
| Modify | `src/lib/Composer.svelte` | Pass-through — `selectedAgent.body` already used |

### Skill

| Action | Path | Responsibility |
|---|---|---|
| Modify | `~/.roy/skills/roy-agent-builder/SKILL.md` | Add scope interview; resolve target via env vars |

### Docker

| Action | Path | Responsibility |
|---|---|---|
| Create | `roy-docker/builtin-agents/roy-agent-builder.md` | Sample built-in (the agent-builder persona itself, for demo) |
| Create | `roy-docker/builtin-agents/roy-coder.md` | Sample built-in |
| Modify | `roy-docker/Dockerfile.roy` | `COPY roy-docker/builtin-agents/ /home/roy/.roy/agents/` |
| Modify | `roy-docker/docker-compose.yml` | Remove `${HOME}/.roy/agents:/home/roy/.roy/agents` bind from `roy-daemon` and `roy-management` |

---

## Sequencing

1. Wire-protocol extension first (T1) — once on disk, both roy and roy-management have the field shape available.
2. Daemon spawn-path plumbing (T2).
3. Management env-var construction (T3).
4. Backend multi-source agents (T4).
5. Backend handler filters by auth (T5).
6. Frontend scope field + chip (T6).
7. Skill update (T7).
8. Docker built-ins (T8).

T6 can run in parallel with T4/T5 if convenient, but the spec stays terminal-driven so verifying multi-scope requires backend changes first.

---

## Task 1: Extend `ClientCommand::Spawn` with `extra_env`

**Files:**
- Modify: `crates/roy/src/control.rs:132-149` (Spawn variant)

- [ ] **Step 1: Add the field**

In `crates/roy/src/control.rs`, locate the `Spawn` variant of `ClientCommand`. Add a new optional field at the end of the field list (after `system_prompt`):

```rust
    Spawn {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        /// Extra environment variables to set on the spawned ACP child.
        /// Used to expose per-user / per-team agent directories to the chat
        /// session (`ROY_AGENTS_DIR_USER`, `ROY_AGENTS_DIR_TEAM_<slug>`,
        /// `ROY_TEAMS`). Empty/absent map is equivalent.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        extra_env: std::collections::HashMap<String, String>,
    },
```

- [ ] **Step 2: Update the wire round-trip tests**

Find the existing round-trip test for `ClientCommand::Spawn` (around `crates/roy/src/control.rs:400-415`). Add one assertion that an extra_env entry survives serialization. Append to one of the existing test functions:

```rust
roundtrip(&ClientCommand::Spawn {
    agent: "claude".to_string(),
    cwd: None,
    model: None,
    permission: None,
    resume: None,
    system_prompt: None,
    extra_env: std::collections::HashMap::from([(
        "ROY_AGENTS_DIR_USER".to_string(),
        "/home/roy/.roy/workspace/users/abc/.roy/agents".to_string(),
    )]),
});
```

- [ ] **Step 3: Build**

Run: `cd /Users/i_strelov/Projects/roy && cargo build -p roy 2>&1 | tail -10`
Expected: errors about every `Spawn { ... }` literal in the workspace missing the new field. List them — Task 2 fixes them.

- [ ] **Step 4: Update all construction sites with `..Default` or explicit empty**

`grep -rn "ClientCommand::Spawn\b\|Spawn {" crates/ | grep -v "ServerEvent::Spawn"` lists construction sites in roy + roy-management + roy-cli. For each, add `extra_env: Default::default()` (or `std::collections::HashMap::new()`) so the field is initialized.

- [ ] **Step 5: Build all**

Run: `cd /Users/i_strelov/Projects/roy && cargo build --workspace 2>&1 | tail -10`
Expected: 0 errors. Tests not yet run.

- [ ] **Step 6: Run wire tests**

Run: `cargo test -p roy control:: 2>&1 | tail -10`
Expected: all pass, including the new extra_env round-trip.

- [ ] **Step 7: Commit**

```bash
cd /Users/i_strelov/Projects/roy
git add -A
git commit -m "feat(wire): extend ClientCommand::Spawn with extra_env map"
```

---

## Task 2: Plumb `extra_env` through to `cmd.env`

**Files:**
- Modify: `crates/roy/src/transport/mod.rs` (Transport trait)
- Modify: `crates/roy/src/transport/acp/mod.rs:182-212`
- Modify: `crates/roy/src/manager.rs` (or whatever owns SessionManager::spawn)
- Modify: `crates/roy/src/daemon.rs` (Spawn handler — pass extra_env through)

- [ ] **Step 1: Locate the `Transport::open` declaration**

Run: `grep -B 1 -A 8 "fn open" crates/roy/src/transport/mod.rs`
Look for the `async fn open(...)` signature in the `Transport` trait. The current signature is:
```rust
async fn open(
    &self,
    session_id: &str,
    resume_cursor: Option<&str>,
    cwd: PathBuf,
    system_prompt: Option<&str>,
) -> Result<Box<dyn Handle>>;
```

- [ ] **Step 2: Add `extra_env` to the trait method**

Edit `Transport::open` in `crates/roy/src/transport/mod.rs` so the signature becomes:

```rust
async fn open(
    &self,
    session_id: &str,
    resume_cursor: Option<&str>,
    cwd: PathBuf,
    system_prompt: Option<&str>,
    extra_env: &std::collections::HashMap<String, String>,
) -> Result<Box<dyn Handle>>;
```

- [ ] **Step 3: Apply `extra_env` inside `AcpTransport::open`**

Edit `crates/roy/src/transport/acp/mod.rs` — the function `impl Transport for AcpTransport { async fn open(...) }`. After the existing `cmd.env("ROY_SESSION_ID", session_id);` line (around line 211), append:

```rust
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
```

Update the parameter list to accept `extra_env: &std::collections::HashMap<String, String>` to match the trait.

- [ ] **Step 4: Update callers of `Transport::open`**

`grep -rn "transport.*open\|\.open(" crates/roy/src/ | head` shows the callers. The main one is in the daemon's Spawn handling, where the call site has a `ClientCommand::Spawn { ..., extra_env, ... }` available — pass `&extra_env`. For internal callers (resume paths, tests), pass `&Default::default()`.

- [ ] **Step 5: Fix tests**

`cargo test -p roy 2>&1 | tail -30` will list test files that call `Transport::open` with the old signature. For each test, add `&Default::default()` as the new argument. Mock implementations of `Transport` need to update their `open` method signature too — same field at the end.

- [ ] **Step 6: Verify**

Run: `cargo test -p roy 2>&1 | tail -10`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(daemon): plumb extra_env from Spawn to AcpTransport"
```

---

## Task 3: Management computes env vars and passes through Spawn

**Files:**
- Modify: `crates/roy-management/src/http.rs` (session create handler)
- Modify: `crates/roy-management/src/roy_client.rs` (Spawn wrapper)
- Modify: `crates/roy-management/src/state.rs` (AppState — add `auth_pool` if needed)

- [ ] **Step 1: Inspect the session create handler**

Run: `grep -n "POST .*sessions\|async fn create_session" crates/roy-management/src/http.rs`
Locate the handler that translates `CreateSessionReq` → `Spawn`. It takes the `Extension<AuthUser>` (the user_id) — confirm by reading the function.

- [ ] **Step 2: Add team lookup**

Inside `create_session`, after extracting `user.0` (user_id), look up teams:

```rust
let team_store = roy_auth::TeamStore::new(s.auth_pool.clone());
let teams = team_store.list_for_user(&user.0).await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
```

`TeamStore::list_for_user` returns `Vec<TeamMembership>`. Each `TeamMembership` has `id`, `name`, `role` (see `crates/roy-auth/src/types.rs:57`).

If `s.auth_pool` doesn't exist yet, check `state.rs`: roy-auth pool may already be passed in alongside the meta_store pool. If not, add it to `AppState`.

- [ ] **Step 3: Build the env-var map**

After the team lookup, build the map. Add a helper in `crates/roy-management/src/agents.rs`:

```rust
pub fn spawn_env_for(
    workspace_dir: &Path,
    user_id: &str,
    teams: &[roy_auth::types::TeamMembership],
) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    let user_dir = workspace_dir
        .join("users")
        .join(user_id)
        .join(".roy/agents");
    env.insert(
        "ROY_AGENTS_DIR_USER".to_string(),
        user_dir.to_string_lossy().to_string(),
    );
    let mut slugs: Vec<String> = Vec::with_capacity(teams.len());
    for t in teams {
        let slug = slugify_team(&t.name);
        let key = format!("ROY_AGENTS_DIR_TEAM_{}", slug.to_ascii_uppercase().replace('-', "_"));
        let dir = workspace_dir
            .join("teams")
            .join(&t.id)
            .join(".roy/agents");
        env.insert(key, dir.to_string_lossy().to_string());
        slugs.push(slug);
    }
    if !slugs.is_empty() {
        env.insert("ROY_TEAMS".to_string(), slugs.join(","));
    }
    env
}

fn slugify_team(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}
```

- [ ] **Step 4: Wire it into the Spawn call**

Where the handler calls `roy_client.spawn(...)`, pass the built env map. The `roy_client::spawn` wrapper (or whatever method sends the `ClientCommand::Spawn` over the WS) needs an `extra_env` parameter.

Open `crates/roy-management/src/roy_client.rs`, find the spawn wrapper, add `extra_env: HashMap<String,String>` to its signature, forward into the `ClientCommand::Spawn { extra_env, ... }` literal.

- [ ] **Step 5: Build + test**

```bash
cd /Users/i_strelov/Projects/roy
cargo build -p roy-management 2>&1 | tail -10
cargo test -p roy-management 2>&1 | tail -15
```
Expected: clean. Fix any signature mismatches.

- [ ] **Step 6: Add a unit test for `spawn_env_for`**

In `crates/roy-management/src/agents.rs` `#[cfg(test)]` block:

```rust
#[test]
fn spawn_env_for_personal_only() {
    let env = spawn_env_for(
        std::path::Path::new("/ws"),
        "user-uuid",
        &[],
    );
    assert_eq!(env["ROY_AGENTS_DIR_USER"], "/ws/users/user-uuid/.roy/agents");
    assert!(!env.contains_key("ROY_TEAMS"));
}

#[test]
fn spawn_env_for_with_teams() {
    use roy_auth::types::TeamMembership;
    let teams = vec![
        TeamMembership { id: "tid-1".into(), name: "GTM Team".into(), role: roy_auth::types::Role::Member },
        TeamMembership { id: "tid-2".into(), name: "Eng".into(), role: roy_auth::types::Role::Owner },
    ];
    let env = spawn_env_for(std::path::Path::new("/ws"), "u", &teams);
    assert_eq!(env["ROY_AGENTS_DIR_TEAM_GTM_TEAM"], "/ws/teams/tid-1/.roy/agents");
    assert_eq!(env["ROY_AGENTS_DIR_TEAM_ENG"], "/ws/teams/tid-2/.roy/agents");
    assert_eq!(env["ROY_TEAMS"], "gtm-team,eng");
}
```

Run: `cargo test -p roy-management spawn_env_for 2>&1 | tail -10`
Expected: 2 tests pass.

If `Role` import differs from what's in roy-auth, adjust to match the actual variants (look at `crates/roy-auth/src/types.rs:57+`).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(management): expose user/team agent dirs as ACP env vars"
```

---

## Task 4: Multi-source agents scanner

**Files:**
- Modify: `crates/roy-management/src/agents.rs`

- [ ] **Step 1: Replace `AgentFile` with scope-tagged shape**

In `crates/roy-management/src/agents.rs`, extend the struct:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AgentScope {
    Builtin,
    Personal,
    Team { team_id: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    pub name: String,
    pub description: String,
    pub engine: String,
    pub model: Option<String>,
    pub body: String,
    pub scope: AgentScope,
}
```

- [ ] **Step 2: Refactor `list_agents_from` into `list_dir` + `list_all_agents`**

Replace the existing `list_agents_from(home)` with a more granular pair:

```rust
async fn list_dir(dir: &Path, scope: AgentScope) -> Vec<AgentFile> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if is_safe_agent_name(s) => s.to_string(),
            _ => continue,
        };
        let Ok(contents) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        let Some(parsed) = parse_agent_md(&contents) else { continue };
        let Some(engine) = parsed.engine else { continue };
        out.push(AgentFile {
            name: parsed.name.unwrap_or(stem),
            description: parsed.description.unwrap_or_default(),
            engine,
            model: parsed.model,
            body: parsed.body,
            scope: scope.clone(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub async fn list_all_agents(
    builtin_dir: &Path,
    workspace_dir: &Path,
    user_id: &str,
    team_ids: &[String],
) -> Vec<AgentFile> {
    let mut out = list_dir(builtin_dir, AgentScope::Builtin).await;
    let user_dir = workspace_dir
        .join("users")
        .join(user_id)
        .join(".roy/agents");
    out.extend(list_dir(&user_dir, AgentScope::Personal).await);
    for tid in team_ids {
        let team_dir = workspace_dir
            .join("teams")
            .join(tid)
            .join(".roy/agents");
        out.extend(
            list_dir(&team_dir, AgentScope::Team { team_id: tid.clone() }).await,
        );
    }
    out
}
```

The order (builtin → personal → team) is preserved by simple concatenation.

- [ ] **Step 3: Update or remove `list_agents_from`**

If any internal caller still uses the old function, point it at `list_all_agents` with appropriate args. The cache (`AgentsCache::get()`) needs to change — see Step 4.

- [ ] **Step 4: Make the cache user-aware**

Replace the `AgentsCache` struct:

```rust
type CacheKey = (String, Vec<String>);  // (user_id, sorted team_ids)

#[derive(Default)]
pub struct AgentsCache {
    inner: Mutex<HashMap<CacheKey, (Instant, Vec<AgentFile>)>>,
}

impl AgentsCache {
    pub async fn get(
        &self,
        builtin_dir: &Path,
        workspace_dir: &Path,
        user_id: &str,
        team_ids: &[String],
    ) -> Vec<AgentFile> {
        let mut tids: Vec<String> = team_ids.to_vec();
        tids.sort();
        let key = (user_id.to_string(), tids.clone());
        {
            let g = self.inner.lock().unwrap();
            if let Some((ts, ref v)) = g.get(&key) {
                if ts.elapsed() < CACHE_TTL {
                    return v.clone();
                }
            }
        }
        let v = list_all_agents(builtin_dir, workspace_dir, user_id, &tids).await;
        let mut g = self.inner.lock().unwrap();
        g.insert(key, (Instant::now(), v.clone()));
        v
    }

    pub fn invalidate(&self) {
        self.inner.lock().unwrap().clear();
    }
}
```

(Add `use std::collections::HashMap;` at the top of agents.rs if not already imported.)

- [ ] **Step 5: Update existing tests**

The v1 tests called `list_agents_from`. Convert them:

```rust
#[tokio::test]
async fn lists_personal_agents() {
    let home = TempDir::new().unwrap();
    let dir = home.path()
        .join("workspace/users/u1/.roy/agents");
    write(&dir, "pirate.md",
        "---\nname: pirate\ndescription: arr\nengine: codex\n---\n\nArr.\n");
    let list = list_all_agents(
        &home.path().join("builtin"),  // empty dir
        &home.path().join("workspace"),
        "u1",
        &[],
    ).await;
    assert_eq!(list.len(), 1);
    assert!(matches!(list[0].scope, AgentScope::Personal));
}

#[tokio::test]
async fn includes_builtin_and_team() {
    let home = TempDir::new().unwrap();
    write(&home.path().join("builtin"), "roy-coder.md",
        "---\nname: roy-coder\nengine: claude\n---\nbody");
    write(&home.path().join("workspace/teams/tid-1/.roy/agents"), "gtm.md",
        "---\nname: gtm\nengine: codex\n---\nbody");
    let list = list_all_agents(
        &home.path().join("builtin"),
        &home.path().join("workspace"),
        "u1",
        &["tid-1".to_string()],
    ).await;
    assert_eq!(list.len(), 2);
    assert!(matches!(list[0].scope, AgentScope::Builtin));
    assert!(matches!(list[1].scope, AgentScope::Team { .. }));
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p roy-management agents:: 2>&1 | tail -15`
Expected: all new + old tests pass.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(agents): multi-source scanner with scope tagging + per-user cache"
```

---

## Task 5: HTTP handler filters by JWT identity

**Files:**
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/src/state.rs` (if `auth_pool` missing)

- [ ] **Step 1: Update the handler**

Find `list_agent_files` (added in v1 plan, now needs scope-aware fetch). Replace its body:

```rust
async fn list_agent_files(
    State(s): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<crate::agents::AgentFile>>, ApiError> {
    let team_store = roy_auth::TeamStore::new(s.auth_pool.clone());
    let teams = team_store
        .list_for_user(&user.0)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let team_ids: Vec<String> = teams.into_iter().map(|t| t.id).collect();
    Ok(Json(
        s.agents_cache
            .get(
                std::path::Path::new("/home/roy/.roy/agents"),
                &s.workspace_dir,
                &user.0,
                &team_ids,
            )
            .await,
    ))
}
```

The built-in path hardcoded as `/home/roy/.roy/agents` is fine for container deployment. Make it overrideable via `ROY_BUILTIN_AGENTS_DIR` env var for tests:

```rust
fn builtin_agents_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ROY_BUILTIN_AGENTS_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from("/home/roy/.roy/agents")
}
```

Use `builtin_agents_dir()` in place of the hardcoded path.

- [ ] **Step 2: Verify `s.auth_pool` exists**

Check `state.rs` for `pub auth_pool: SqlitePool` (or similar). If absent, add it:

```rust
pub auth_pool: sqlx::SqlitePool,
```

And populate it in `AppState` construction (lib.rs) — the auth pool may already be the same `pool` shared with meta_store; check existing usage. If they share the pool, just reference `s.pool` (or whatever the existing field is) directly.

- [ ] **Step 3: Update the smoke test from v1**

The v1 test `list_agent_files_returns_files_from_home` set `HOME=tempdir` and put files at `~/.roy/agents/`. That path is now the BUILTIN dir. Rename the test and switch the env var to `ROY_BUILTIN_AGENTS_DIR`:

```rust
#[tokio::test]
async fn list_agent_files_returns_builtin_entries() {
    let (router, home, _state_guard) = test_router().await;
    let builtin = home.path().join("builtin-agents");
    std::fs::create_dir_all(&builtin).unwrap();
    std::fs::write(
        builtin.join("roy-coder.md"),
        "---\nname: roy-coder\ndescription: helper\nengine: claude\n---\n\nbody",
    )
    .unwrap();
    std::env::set_var("ROY_BUILTIN_AGENTS_DIR", &builtin);
    // ... rest mirrors v1
}
```

Add a second test for personal+team agents — populate dirs inside the test workspace, assert scope tags in the response.

- [ ] **Step 4: Build + test**

```bash
cargo build -p roy-management 2>&1 | tail -5
cargo test -p roy-management 2>&1 | tail -20
```
Expected: clean. Frontend type-check waits until T6.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(management): /agents endpoint filters by JWT identity"
```

---

## Task 6: Frontend scope field + chip

**Files:**
- Modify: `src/lib/agents.svelte.ts`
- Modify: `src/lib/AgentsView.svelte`

Working dir: `/Users/i_strelov/Projects/roy-web` (or its worktree).

- [ ] **Step 1: Extend the `Agent` type**

Edit `src/lib/agents.svelte.ts`:

```ts
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

type WireAgent = {
  name: string;
  description: string;
  engine: string;
  model?: string | null;
  body: string;
  scope: { kind: 'builtin' } | { kind: 'personal' } | { kind: 'team'; team_id: string };
};
```

Update the parse mapper in `load()` to copy `scope` through:

```ts
.map((a) => ({
  name: a.name,
  description: a.description,
  engine: a.engine,
  model: a.model ?? undefined,
  body: a.body,
  scope: a.scope,
}));
```

Add a defensive filter that drops entries with an unrecognized `scope.kind`:

```ts
.filter((a) => {
  if (a.scope.kind === 'builtin' || a.scope.kind === 'personal' || a.scope.kind === 'team') return true;
  // eslint-disable-next-line no-console
  console.warn(`agent ${a.name}: unknown scope kind, skipping`);
  return false;
})
```

- [ ] **Step 2: Sort by scope then name**

In the same file, expose a sorted-list derived value:

```ts
import { authState } from './auth.svelte';

// (inside or after the AgentsState class — sort happens at consumer level)
function scopeRank(s: AgentScope): number {
  if (s.kind === 'builtin') return 0;
  if (s.kind === 'personal') return 1;
  return 2;
}

export function sortAgents(list: Agent[]): Agent[] {
  return [...list].sort((a, b) => {
    const r = scopeRank(a.scope) - scopeRank(b.scope);
    if (r !== 0) return r;
    return a.name.localeCompare(b.name);
  });
}
```

- [ ] **Step 3: Render scope chip on cards**

Edit `src/lib/AgentsView.svelte`. Inside the `{#each filtered}` block, replace the existing engine chip with a row containing both engine and scope chips. Just before each card's existing `engine` chip, add a scope chip:

```svelte
<span class="flex shrink-0 items-center gap-1 rounded-full bg-muted/60 px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground">
  {#if a.scope.kind === 'builtin'}
    roy
  {:else if a.scope.kind === 'personal'}
    personal
  {:else}
    team · {teamName(a.scope.team_id)}
  {/if}
</span>
```

Add the `teamName` helper near the top of the `<script>`:

```ts
import { authState } from './auth.svelte';

function teamName(id: string): string {
  const t = authState.user?.teams.find((x) => x.id === id);
  return t?.name ?? `team · ${id.slice(0, 8)}`;
}
```

- [ ] **Step 4: Apply the new sort**

Replace the `filtered` derived's source from `agentsStore.list` to `sortAgents(agentsStore.list)`. Import `sortAgents` from `./agents.svelte`.

- [ ] **Step 5: Run checks**

```bash
cd /Users/i_strelov/Projects/roy-web
npm run check 2>&1 | tail -5
npm run build 2>&1 | tail -5
```
Expected: 0 errors. Pre-existing autofocus warning OK.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(agents): scope chip + builtin/personal/team sort"
```

---

## Task 7: Update the `roy-agent-builder` skill

**Files:**
- Modify: `~/.roy/skills/roy-agent-builder/SKILL.md`

This skill file lives on the host. After editing, the daemon's 30-second cache will pick it up automatically.

- [ ] **Step 1: Add scope interview to the Process section**

Replace the existing Process section (steps 1-5) with:

```markdown
## Process

Interview the user one focused question at a time. Don't dump a multi-part questionnaire — gather just enough to draft, then iterate.

1. **Purpose.** What should this agent do? Who is the user? What does success look like?
2. **Tone & scope.** Voice (formal, terse, playful)? Anything it should refuse?
3. **Engine.** Recommend `claude` for general dialogue, `codex` for code-heavy work, `gemini` for long context, `opencode` for autonomous coding, `pi` for the Pi assistant. Default usually claude.
4. **Model.** Only ask if the user cares. Otherwise leave blank.
5. **Slug.** Derive from purpose (`marketing-strategist`, `bug-triager`).
6. **Scope.** Read `$ROY_TEAMS` env var. If set (comma-separated team slugs), ask: "Save as personal or to one of these teams: <list>?" If `$ROY_TEAMS` is unset/empty, default to personal — don't ask.
```

- [ ] **Step 2: Replace the Writing the file section**

Replace the existing section with:

```markdown
## Writing the file

Resolve the target directory via env vars:
- **Personal:** `$ROY_AGENTS_DIR_USER`
- **Team `<slug>`:** `$ROY_AGENTS_DIR_TEAM_<SLUG_UPPER>` (slug uppercased, dashes → underscores)
- **Built-in `roy-*`:** out of scope. Refuse with "Built-in agents are part of the daemon image. Edit `roy-docker/builtin-agents/<slug>.md` and rebuild."

Use Bash to expand the env var and confirm the full path before writing:

```bash
echo "$ROY_AGENTS_DIR_USER/<slug>.md"
```

If `$ROY_AGENTS_DIR_USER` itself is unset, the daemon hasn't been updated — tell the user to rebuild the daemon image (roy-docker/Dockerfile.roy) and surface the error.

Use the Write tool with the printed absolute path. Create the parent directory first via Bash if it doesn't exist:

```bash
mkdir -p "$(dirname $ROY_AGENTS_DIR_USER/<slug>.md)"
```

Refuse to overwrite an existing file. For edits, Read the current file, propose a diff, then Write with the edited content.
```

- [ ] **Step 3: Update the After writing section**

Add a note about scope:

```markdown
## After writing

- Confirm the path: e.g., `~/.roy/workspace/users/<uid>/.roy/agents/<slug>.md` for personal, or `~/.roy/workspace/teams/<tid>/.roy/agents/<slug>.md` for team.
- Tell the user: refresh `/agents` in the roy web app — the card appears within 30 seconds (server cache TTL). Hit Run to spawn a chat with the persona pre-loaded.
- Personal agents only show up for this user; team agents show for every member of the team.
```

- [ ] **Step 4: Update the Not your job section**

Add:

```markdown
- Editing built-in `roy-*` agents (those are shipped via `roy-docker/builtin-agents/`).
- Determining a user_id or team_id manually — that information lives in env vars set by the daemon at spawn time.
```

- [ ] **Step 5: Verify**

The skill file is plain markdown — no tests. Sanity check: `cat ~/.roy/skills/roy-agent-builder/SKILL.md | head -30` to confirm the frontmatter is intact (no accidental YAML breakage). The frontmatter must still be:
```yaml
---
name: roy-agent-builder
description: ...
---
```

- [ ] **Step 6: This file isn't in any git repo (lives in $HOME). No commit needed.**

---

## Task 8: Built-in agents in the Docker image

**Files:**
- Create: `roy-docker/builtin-agents/roy-coder.md`
- Modify: `roy-docker/Dockerfile.roy`
- Modify: `roy-docker/docker-compose.yml`

Working dir: `/Users/i_strelov/Projects/roy-docker`.

- [ ] **Step 1: Create the built-in catalog**

Create the directory and one sample file:

```bash
mkdir -p /Users/i_strelov/Projects/roy-docker/builtin-agents
```

Create `/Users/i_strelov/Projects/roy-docker/builtin-agents/roy-coder.md`:

```markdown
---
name: roy-coder
description: General coding helper that follows DRY, YAGNI, and TDD.
engine: codex
---

You are a precise senior engineer. Your job:
- Implement what's requested, nothing more.
- Use existing patterns from the codebase before introducing new ones.
- Write tests for non-trivial logic. Prefer integration tests over unit tests when the unit boundary is unclear.
- Commit logical chunks. Never silently fail or swallow errors.

Style:
- Terse, direct, no preamble.
- Show file paths and line numbers when referencing existing code.
- When a request is ambiguous, ask one focused question; don't speculate.

Refuse to:
- Make sweeping refactors not asked for.
- Skip tests "because the change is small."
```

- [ ] **Step 2: Update the Dockerfile**

Edit `/Users/i_strelov/Projects/roy-docker/Dockerfile.roy`. In the runtime stage (Stage 2), AFTER the `useradd roy && mkdir -p /home/roy/...` block (around line 61-66) and BEFORE `USER roy` (around line 74), insert:

```dockerfile
# Built-in agent personas. Read-only for end users — updates go through
# image rebuild. Lives outside the workspace so user/team agent dirs
# (under workspace/users|teams/.../.roy/agents) don't collide.
COPY roy-docker/builtin-agents/ /home/roy/.roy/agents/
RUN chown -R roy:roy /home/roy/.roy/agents
```

- [ ] **Step 3: Remove the v1 bind mount**

Edit `/Users/i_strelov/Projects/roy-docker/docker-compose.yml`. Two services have the `${HOME}/.roy/agents:/home/roy/.roy/agents` bind mount (added during v1 deployment): `roy-daemon` (around line 61) and `roy-management` (around line 92). Delete BOTH lines along with their preceding comments.

After the edit, `roy-daemon`'s volumes section should NOT contain `${HOME}/.roy/agents`, and the comment "Agents catalog: ..." is also gone. Same for `roy-management`.

- [ ] **Step 4: Rebuild images**

```bash
cd /Users/i_strelov/Projects/roy-docker
docker compose build roy-daemon roy-management roy-gateway 2>&1 | tail -8
docker compose build roy-web 2>&1 | tail -5
docker compose up -d --force-recreate 2>&1 | tail -10
```
Expected: all services running.

- [ ] **Step 5: Sanity check**

```bash
# Built-ins reachable inside the container
docker exec roy-daemon ls /home/roy/.roy/agents/

# Personal dir created by the daemon at session creation (will exist after first chat)
docker exec roy-daemon ls /home/roy/.roy/workspace/users/ 2>/dev/null
```

Expected: built-in files visible. Workspace users/ may or may not exist depending on whether a user has chatted post-rebuild.

- [ ] **Step 6: Manual walkthrough**

Open `http://localhost:8080/agents` in the browser, logged in. Expected:
1. The `roy-coder` card appears (built-in, scope chip `roy`).
2. Open a chat, invoke `/roy-agent-builder`, say "create a personal pirate agent in codex".
3. Skill writes to `$ROY_AGENTS_DIR_USER/pirate.md`.
4. Refresh `/agents` — pirate card appears with `personal` chip.
5. If the user belongs to a team, repeat with "save to team <name>" and confirm a `team · <name>` chip appears.

If `ROY_TEAMS` is unset / empty, the team flow is untested — that's expected if the test user has no team memberships.

- [ ] **Step 7: Walkthrough — what's NOT working signals**

If `pirate` doesn't appear:
- `docker exec roy-daemon env | grep ROY_AGENTS` inside an existing session — confirm env vars present. If empty, T3 wasn't applied / daemon not restarted.
- `docker exec roy-daemon cat /home/roy/.roy/workspace/users/<uid>/.roy/agents/pirate.md` — confirm file written. If absent, skill didn't write; check the chat transcript.
- `curl -b "<cookie>" http://localhost:8080/management/agents` — confirm the endpoint returns the personal entry. If yes but UI doesn't render, T6 issue.

- [ ] **Step 8: Commit**

`roy-docker` isn't a git repo (verified earlier), so just leave the files in place. The Dockerfile + docker-compose + builtin-agents/ dir together form the deployable state.

(If the user later wants `roy-docker` under git, that's a separate task.)

---

## Self-review

**Spec coverage:**

- Storage layout (builtin/users/teams paths) — Tasks 4, 5, 8.
- `AgentFile` scope field — Task 4.
- HTTP handler filters by JWT identity — Task 5.
- Env vars `ROY_AGENTS_DIR_USER` / `ROY_AGENTS_DIR_TEAM_<slug>` / `ROY_TEAMS` — Task 3 (compute + forward) + Task 2 (apply to ACP child).
- Wire change `extra_env` on Spawn — Task 1.
- Frontend `Agent` shape + scope chip + sort — Task 6.
- Skill interview + Bash env-var resolution — Task 7.
- Built-in delivery via Dockerfile COPY — Task 8.
- Bind-mount removal — Task 8.

**Placeholder scan:** No TBD/TODO. Every step is concrete. Tests have explicit assertions.

**Type consistency:**
- `AgentScope` Rust enum (Task 4) is `{ Builtin, Personal, Team { team_id } }`. TypeScript mirror (Task 6) uses tagged union `{ kind: 'builtin' }` etc. The wire shape uses `#[serde(tag = "kind", rename_all = "lowercase")]`, which serializes `Builtin` → `{"kind":"builtin"}` and `Team { team_id }` → `{"kind":"team","team_id":"..."}`. Matches.
- `extra_env` type is `HashMap<String, String>` everywhere — Rust trait, Spawn wire, AcpTransport.
- Env var keys consistent across Task 3 (compute), Task 7 (consume): `ROY_AGENTS_DIR_USER`, `ROY_AGENTS_DIR_TEAM_<SLUG_UPPER>`, `ROY_TEAMS`.

**Scope check:** Single coherent feature (three-tier agents). One spec → one plan. Tasks decompose cleanly along architectural seams (wire → daemon → management → frontend → skill → docker).
