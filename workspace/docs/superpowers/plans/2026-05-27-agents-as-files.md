# Agents as files — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the DB-backed agent CRUD + chat-driven Agent Builder with file-based agents at `~/.roy/agents/<name>.md`. Web's `/agents` mirrors `/skills` (read-only catalog + Run button). Delete the `roy-agents` crate, the builder bot, and the management `/agents/*` endpoints.

**Architecture:** Backend gains a new `GET /management/agents` endpoint that scans `~/.roy/agents/*.md`, parses frontmatter (`name`, `description`, `engine`, optional `model`), returns each agent with its body inline. The `roy-agents` crate is deleted; `default_db_path` moves to `roy-management::db`. Frontend gets a new `agentsStore` reading the new endpoint; `AgentsView` is rewritten to look like `SkillsView`; the Agents tab in `ModelPicker` and the persona-attach in `Composer` switch to the new store. Builder route, builder-session LS key, and the entire chat-driven builder UX are deleted.

**Tech Stack:** Two repos.
- `/Users/i_strelov/Projects/roy` — Rust workspace (axum 0.8, sqlx, tokio). Tests via `cargo test`.
- `/Users/i_strelov/Projects/roy-web` — Svelte 5 + Vite + TypeScript. Verification via `npm run check` and manual run.

**Spec:** `docs/superpowers/specs/2026-05-27-agents-as-files-design.md`

---

## File map

### roy (Rust)

| Action | Path | Responsibility |
|---|---|---|
| Create | `crates/roy-management/src/agents.rs` | File-based scanner: `AgentFile`, `list_agents_from`, `roy_agents_dir`, frontmatter parse |
| Create | `crates/roy-management/src/db.rs` | Relocated `default_db_path` + `open` helpers |
| Create | `crates/roy-management/migrations/sqlite/0006_drop_legacy_agents.sql` | `DROP TABLE IF EXISTS agents` cleanup |
| Modify | `crates/roy-management/src/lib.rs` | Drop `roy_agents::Store` from AppState init, expose new `db` + `agents` modules |
| Modify | `crates/roy-management/src/state.rs` | Drop `Store` field from `AppState` |
| Modify | `crates/roy-management/src/http.rs` | Delete `/agents/*` CRUD + builder handlers, add `GET /agents` reading the new module |
| Modify | `crates/roy-management/Cargo.toml` | Drop `roy-agents` dependency |
| Modify | `crates/roy-cli/src/auth.rs:50` | Switch `roy_agents::default_db_path()` → `roy_management::db::default_db_path()` |
| Modify | `crates/roy-cli/src/management_client.rs` | Delete `list/get/create/update/delete/run` agent helpers + types |
| Modify | `crates/roy-cli/src/management.rs` | Delete subcommands that called those helpers (if any) |
| Modify | `crates/roy-cli/Cargo.toml` | Drop direct `roy-agents` dep if present, ensure `roy-management` is a dep |
| Delete | `crates/roy-agents/` (entire crate) | Whole directory + `Cargo.toml` member entry in workspace |
| Modify | `Cargo.toml` (workspace root) | Remove `crates/roy-agents` from `members = [...]` |

### roy-web (TypeScript / Svelte)

| Action | Path | Responsibility |
|---|---|---|
| Create | `src/lib/agents.svelte.ts` | New file-based agents store, `GET /management/agents` |
| Modify | `src/lib/AgentsView.svelte` | Rewrite to mirror `SkillsView.svelte` (cards + modal + Run) |
| Modify | `src/lib/ModelPicker.svelte` | Replace `management-agents` import with `agentsStore`, rename `a.preset`/`a.id` consumers to `a.engine`/`a.name` |
| Modify | `src/lib/Composer.svelte` | `selectedAgent` reads `agentsStore`, identifier becomes agent `name` (string), persona forwarding unchanged |
| Modify | `src/lib/App.svelte` | Delete builder route (`/agents/<id>` parse + `applyRoute` builder branch + `openBuilder` + render), drop `AgentBuilderView` import |
| Modify | `src/lib/SessionList.svelte` | Drop builder-session marker (if present) |
| Modify | `src/lib/management-client.ts` | Delete `Agent`/`NewAgent`/`AgentPatch`/`StartBuilderResp` types, `TAG_BUILDER_AGENT_ID`, `management.{list,get,create,update,remove,run,startBuilder}` |
| Modify | `src/lib/utils.ts:LS` | Drop `builderSession` factory |
| Delete | `src/lib/AgentBuilderView.svelte` | The chat-driven builder UI |
| Delete | `src/lib/agent-builder-store.svelte.ts` | Polling store for the builder session |
| Delete | `src/lib/management-agents.svelte.ts` | Old DB-backed agents store (replaced by `agents.svelte.ts`) |

---

## Sequencing

Backend first so the new endpoint exists before the SPA tries to use it. CLI cleanup right after backend (depends on relocated `default_db_path`). Frontend last — depends on the new endpoint being live.

---

## Task 1: Relocate `default_db_path` to roy-management

**Files:**
- Create: `crates/roy-management/src/db.rs`
- Modify: `crates/roy-management/src/lib.rs`
- Modify: `crates/roy-cli/src/auth.rs:50`
- Modify: `crates/roy-cli/Cargo.toml` (add `roy-management` dep if missing)

This is the first step because every other backend task assumes the new path is in place.

- [ ] **Step 1: Inspect the current `default_db_path` to know what to copy**

Run: `cat /Users/i_strelov/Projects/roy/crates/roy-agents/src/db.rs`
Expected: file with `pub fn default_db_path() -> PathBuf` and `pub async fn open(...) -> Result<SqlitePool, ...>`. Copy the full bodies of both functions; you'll paste them into the new file unchanged.

- [ ] **Step 2: Create `crates/roy-management/src/db.rs`**

Paste the *exact* `default_db_path` and `open` functions from `roy-agents/src/db.rs`. Adjust the `sqlx::migrate!` path in `open` to point at `migrations/sqlite` (the path is relative to the crate root, so it now resolves to `roy-management/migrations/sqlite`). That said, `open` here is only for the agents table — the management crate already opens the DB elsewhere via `meta_store::MetaStore::apply_migrations`, so we don't need to migrate from inside `db.rs`. Keep `default_db_path` only. Drop `open`.

```rust
// crates/roy-management/src/db.rs
//
// Shared SQLite path for roy-cli + roy-management. Was previously in the
// roy-agents crate; that crate has been deleted now that agents live in
// `~/.roy/agents/*.md` files.

use std::path::PathBuf;

/// Returns the canonical SQLite path used by roy-management state.
/// Defaults to `~/.local/state/roy/management.sqlite`; honours `ROY_DB`
/// for tests and ad-hoc deployments.
pub fn default_db_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_DB") {
        return PathBuf::from(s);
    }
    let home = dirs::home_dir().expect("home dir resolvable");
    home.join(".local/state/roy/management.sqlite")
}
```

(Confirm against the actual roy-agents/db.rs body — if it differs from this, copy what's actually there. The env-var name and path are what matter; keep them identical.)

- [ ] **Step 3: Wire the new module into the crate**

Edit `crates/roy-management/src/lib.rs`. Add at the top of the `pub mod` block (alphabetical with the others):

```rust
pub mod db;
```

- [ ] **Step 4: Update `roy-cli/src/auth.rs`**

`crates/roy-cli/src/auth.rs:50` currently reads:
```rust
let db = roy_agents::default_db_path();
```
Change to:
```rust
let db = roy_management::db::default_db_path();
```

- [ ] **Step 5: Ensure `roy-management` is a dep of `roy-cli`**

Check `crates/roy-cli/Cargo.toml`. If `roy-management = { path = "../roy-management" }` is NOT present in `[dependencies]`, add it. If it's present, no change.

- [ ] **Step 6: Verify the workspace still builds**

Run: `cd /Users/i_strelov/Projects/roy && cargo build -p roy-cli -p roy-management 2>&1 | tail -20`
Expected: build success. `roy-agents` is still in the workspace and still depended on by `roy-management` — that comes next.

- [ ] **Step 7: Commit**

```bash
cd /Users/i_strelov/Projects/roy
git add crates/roy-management/src/db.rs crates/roy-management/src/lib.rs crates/roy-cli/src/auth.rs crates/roy-cli/Cargo.toml
git commit -m "refactor: relocate default_db_path from roy-agents to roy-management"
```

---

## Task 2: New `agents` module — file scanner

**Files:**
- Create: `crates/roy-management/src/agents.rs`
- Modify: `crates/roy-management/src/lib.rs`

- [ ] **Step 1: Create `crates/roy-management/src/agents.rs`**

```rust
//! Filesystem-based agent discovery. Single source:
//!
//!   - `~/.roy/agents/<name>.md` — top-level markdown files, one per agent.
//!
//! Each file starts with a YAML frontmatter block. Required keys: `name`,
//! `description`, `engine`. Optional: `model`. The body (after the second
//! `---`) becomes the session's `system_prompt` when the agent is run.
//!
//! Files without `engine` are silently dropped — that is what distinguishes
//! an agent file from a stray markdown note in the same directory.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    pub name: String,
    pub description: String,
    pub engine: String,
    pub model: Option<String>,
    pub body: String,
}

const CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct AgentsCache {
    inner: Mutex<Option<(Instant, Vec<AgentFile>)>>,
}

impl AgentsCache {
    pub async fn get(&self) -> Vec<AgentFile> {
        {
            let g = self.inner.lock().unwrap();
            if let Some((ts, ref v)) = *g {
                if ts.elapsed() < CACHE_TTL {
                    return v.clone();
                }
            }
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let v = list_agents_from(&home).await;
        let mut g = self.inner.lock().unwrap();
        *g = Some((Instant::now(), v.clone()));
        v
    }

    pub fn invalidate(&self) {
        *self.inner.lock().unwrap() = None;
    }
}

pub fn roy_agents_dir(home: &Path) -> PathBuf {
    home.join(".roy/agents")
}

pub async fn list_agents_from(home: &Path) -> Vec<AgentFile> {
    let dir = roy_agents_dir(home);
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
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
        let Some(parsed) = parse_agent_md(&contents) else {
            continue;
        };
        // Only emit if engine is present — that is what marks the file as
        // an agent vs a stray note.
        let Some(engine) = parsed.engine else { continue };
        out.push(AgentFile {
            // The filename stem wins over frontmatter `name` if they disagree —
            // routing is filename-based.
            name: parsed.name.unwrap_or(stem.clone()),
            description: parsed.description.unwrap_or_default(),
            engine,
            model: parsed.model,
            body: parsed.body,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

struct ParsedAgent {
    name: Option<String>,
    description: Option<String>,
    engine: Option<String>,
    model: Option<String>,
    body: String,
}

fn parse_agent_md(s: &str) -> Option<ParsedAgent> {
    let s = s.strip_prefix("---\n")?;
    let end = s.find("\n---")?;
    let front = &s[..end];
    let after = &s[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after).to_string();
    let (mut name, mut desc, mut engine, mut model) = (None, None, None, None);
    for line in front.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            desc = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("engine:") {
            engine = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("model:") {
            model = Some(rest.trim().trim_matches('"').to_string());
        }
    }
    Some(ParsedAgent { name, description: desc, engine, model, body })
}

fn is_safe_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[tokio::test]
    async fn lists_agents_in_alphabetical_order() {
        let home = TempDir::new().unwrap();
        let dir = roy_agents_dir(home.path());
        write(
            &dir,
            "pirate.md",
            "---\nname: pirate\ndescription: pirate coder\nengine: codex\n---\n\nArr.\n",
        );
        write(
            &dir,
            "marketing.md",
            "---\nname: marketing\ndescription: gtm helper\nengine: claude\nmodel: claude-opus-4-7\n---\n\nYou are a marketer.\n",
        );
        let list = list_agents_from(home.path()).await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "marketing");
        assert_eq!(list[0].engine, "claude");
        assert_eq!(list[0].model.as_deref(), Some("claude-opus-4-7"));
        assert!(list[0].body.contains("You are a marketer"));
        assert_eq!(list[1].name, "pirate");
        assert_eq!(list[1].engine, "codex");
        assert_eq!(list[1].model, None);
    }

    #[tokio::test]
    async fn skips_files_without_engine_field() {
        let home = TempDir::new().unwrap();
        let dir = roy_agents_dir(home.path());
        write(
            &dir,
            "skill-only.md",
            "---\nname: notes\ndescription: just a note\n---\n\nbody\n",
        );
        let list = list_agents_from(home.path()).await;
        assert_eq!(list.len(), 0);
    }

    #[tokio::test]
    async fn rejects_unsafe_names() {
        let home = TempDir::new().unwrap();
        let dir = roy_agents_dir(home.path());
        write(
            &dir,
            "../escape.md",
            "---\nname: x\nengine: claude\n---\n\nx",
        );
        // The `..` segment isn't a file extension we collect; the entry's
        // stem fails `is_safe_agent_name`. Result: empty list.
        let list = list_agents_from(home.path()).await;
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn parses_minimal_frontmatter() {
        let p = parse_agent_md(
            "---\nname: x\nengine: claude\n---\n\nhello\n",
        )
        .unwrap();
        assert_eq!(p.name.as_deref(), Some("x"));
        assert_eq!(p.engine.as_deref(), Some("claude"));
        assert_eq!(p.body.trim(), "hello");
        assert!(p.description.is_none());
        assert!(p.model.is_none());
    }
}
```

- [ ] **Step 2: Register the module**

Edit `crates/roy-management/src/lib.rs`. Add `pub mod agents;` near the other `pub mod` declarations.

- [ ] **Step 3: Wire the cache into `AppState`**

Edit `crates/roy-management/src/state.rs`. Add a field on `AppState`:

```rust
pub agents_cache: std::sync::Arc<crate::agents::AgentsCache>,
```

Then in `crates/roy-management/src/lib.rs` where `AppState` is constructed (look for `commands_cache: Arc::new(...)`), add a sibling line:

```rust
agents_cache: Arc::new(crate::agents::AgentsCache::default()),
```

- [ ] **Step 4: Run the new tests**

Run: `cd /Users/i_strelov/Projects/roy && cargo test -p roy-management agents::tests 2>&1 | tail -20`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
cd /Users/i_strelov/Projects/roy
git add crates/roy-management/src/agents.rs crates/roy-management/src/lib.rs crates/roy-management/src/state.rs
git commit -m "feat(management): file-based agents scanner with cache"
```

---

## Task 3: HTTP endpoint `GET /management/agents`

**Files:**
- Modify: `crates/roy-management/src/http.rs`

This task only adds the new endpoint. Old `/agents/*` handlers still exist; Task 6 removes them.

- [ ] **Step 1: Add the handler**

At the bottom of `crates/roy-management/src/http.rs` (after the other handler fns, before the `#[cfg(test)]` block), add:

```rust
async fn list_agent_files(
    State(s): State<AppState>,
) -> Json<Vec<crate::agents::AgentFile>> {
    Json(s.agents_cache.get().await)
}
```

- [ ] **Step 2: Mount the route**

In the same file, in the `router()` function, find the existing `.route("/agents", ...)` line in the protected router. The mounting plan: this task uses a TEMPORARY path so the old endpoint stays working until Task 6 deletes it. Pick `/agents-files` (hyphen) as the temp path:

Find the existing block (around line 58-65):
```rust
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/_builder", post(start_builder))
        .route(
            "/agents/{id}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/agents/{id}/run", post(run_agent))
```

Add a single new line in the same chain, just above the `/agents` line:
```rust
        .route("/agents-files", get(list_agent_files))
```

- [ ] **Step 3: Add a smoke test**

In `crates/roy-management/src/http.rs`, in the `#[cfg(test)] mod tests` block (find an existing test as a template — look for `async fn lists_agents` or similar), append:

```rust
#[tokio::test]
async fn list_agent_files_returns_parsed_entries() {
    let (router, _home, _state_guard) = test_router().await;
    // ^^ if `test_router` doesn't exist verbatim, look for the existing
    // helper that builds an axum Router with an `AppState`. Most other
    // tests in this file use one. If you need to drop a fixture file
    // into the home tempdir, do it before calling `oneshot`.

    std::fs::create_dir_all(_home.path().join(".roy/agents")).unwrap();
    std::fs::write(
        _home.path().join(".roy/agents/pirate.md"),
        "---\nname: pirate\ndescription: arr\nengine: codex\n---\n\nArr.",
    )
    .unwrap();

    let resp = router
        .oneshot(
            Request::get("/agents-files")
                .header("cookie", "roy_session=test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let agents: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], "pirate");
    assert_eq!(agents[0]["engine"], "codex");
}
```

If the existing test pattern uses a different fixture name (`test_router`, `build_app`, etc.), adapt the call. The shape — POST a file into tempdir, GET the endpoint, assert JSON — stays the same.

- [ ] **Step 4: Run the new test**

Run: `cd /Users/i_strelov/Projects/roy && cargo test -p roy-management list_agent_files_returns_parsed_entries 2>&1 | tail -15`
Expected: 1 test pass.

- [ ] **Step 5: Build**

Run: `cd /Users/i_strelov/Projects/roy && cargo build -p roy-management 2>&1 | tail -5`
Expected: success.

- [ ] **Step 6: Commit**

```bash
cd /Users/i_strelov/Projects/roy
git add crates/roy-management/src/http.rs
git commit -m "feat(management): GET /agents-files endpoint (temp path)"
```

---

## Task 4: Frontend — new `agentsStore` consuming the temp endpoint

Switching to the roy-web repo. Implementer should `cd /Users/i_strelov/Projects/roy-web` from here on for these tasks.

**Files:**
- Create: `src/lib/agents.svelte.ts`

- [ ] **Step 1: Create the new store**

```ts
// src/lib/agents.svelte.ts
//
// File-based agents store. Reads /management/agents-files (a server-side
// scan of ~/.roy/agents/*.md). Client filters out entries whose `engine`
// isn't in the AgentPreset union — corrupted files don't crash render,
// they just don't appear in the catalog (with a console warning).

import type { AgentPreset } from './wire';
import { KNOWN_PRESETS } from './wire';

export type Agent = {
  name: string;
  description: string;
  engine: AgentPreset;
  model?: string;
  body: string;
};

type WireAgent = {
  name: string;
  description: string;
  engine: string;
  model?: string | null;
  body: string;
};

class AgentsState {
  list = $state<Agent[]>([]);
  loading = $state(false);
  loaded = $state(false);
  error = $state<string | null>(null);

  async load(force = false): Promise<void> {
    if ((this.loaded || this.loading) && !force) return;
    this.loading = true;
    this.error = null;
    try {
      const res = await fetch('/management/agents-files', { credentials: 'include' });
      if (!res.ok) {
        if (res.status === 401) {
          this.list = [];
          return;
        }
        throw new Error(`HTTP ${res.status}`);
      }
      const raw = (await res.json()) as WireAgent[];
      this.list = raw
        .filter((a): a is WireAgent & { engine: AgentPreset } => {
          if (KNOWN_PRESETS.has(a.engine as AgentPreset)) return true;
          // eslint-disable-next-line no-console
          console.warn(`agent ${a.name}: unknown engine "${a.engine}", skipping`);
          return false;
        })
        .map((a) => ({
          name: a.name,
          description: a.description,
          engine: a.engine,
          model: a.model ?? undefined,
          body: a.body,
        }));
      this.loaded = true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }
}

export const agentsStore = new AgentsState();
```

- [ ] **Step 2: Verify types**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run check 2>&1 | tail -5`
Expected: 0 errors.

- [ ] **Step 3: Commit (roy-web)**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/agents.svelte.ts
git commit -m "feat(agents): file-based agents store"
```

---

## Task 5: Rewrite `AgentsView.svelte` — cards + modal + Run

**Files:**
- Modify: `src/lib/AgentsView.svelte` (full rewrite)

- [ ] **Step 1: Replace the file contents**

Replace the entire `src/lib/AgentsView.svelte` with the version below. It mirrors `SkillsView.svelte`'s layout (header, search, grid, modal) but with engine/model chips and a Run button that calls `app.createSession`.

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { Bot, Play, RefreshCw, Search } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import * as Dialog from '$lib/components/ui/dialog';
  import { agentsStore, type Agent } from './agents.svelte';
  import { app } from './state.svelte';
  import { enginesConfig, defaultModelFor } from './engines-config.svelte';
  import ProviderIcon from './ProviderIcon.svelte';
  import { agentIcon } from './provider-icons';
  import type { AgentPreset } from './wire';

  let {
    onOpenSession,
  }: {
    onOpenSession?: (id: string) => void;
  } = $props();

  let query = $state('');
  let selected = $state<Agent | null>(null);
  let running = $state<string | null>(null);

  onMount(() => {
    void agentsStore.load();
    enginesConfig.refresh();
  });

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return agentsStore.list;
    return agentsStore.list.filter(
      (a) =>
        a.name.toLowerCase().includes(q) ||
        a.description.toLowerCase().includes(q),
    );
  });

  async function run(a: Agent) {
    if (running) return;
    const resolved =
      a.model ??
      defaultModelFor(enginesConfig.engines, a.engine)?.id;
    if (!resolved) {
      app.lastError = `Agent "${a.name}": engine "${a.engine}" not in the engines catalog.`;
      return;
    }
    running = a.name;
    try {
      const sessionId = await app.createSession({
        agent: a.engine as AgentPreset,
        model: resolved,
        persona: { prompt: a.body, name: a.name },
      });
      selected = null;
      onOpenSession?.(sessionId);
    } catch (e) {
      app.lastError = (e as Error).message;
    } finally {
      running = null;
    }
  }
</script>

<div class="flex h-full min-h-0 w-full flex-col bg-background">
  <header class="border-b border-border/40 bg-background/95 px-6 py-4 backdrop-blur">
    <div class="flex items-center justify-between gap-3">
      <div class="flex items-center gap-2.5">
        <Bot class="size-5 text-muted-foreground" />
        <div>
          <h1 class="text-lg font-semibold text-foreground">Agents</h1>
          <p class="text-xs text-muted-foreground">
            Personas stored as markdown files under
            <code class="mx-1 rounded bg-muted px-1.5 py-0.5 font-mono text-[11px]">~/.roy/agents/</code>.
            Click a card to inspect, hit Run to spawn a chat.
          </p>
        </div>
      </div>
      <Button
        variant="ghost"
        size="icon"
        onclick={() => void agentsStore.load(true)}
        disabled={agentsStore.loading}
        title="Refresh"
        aria-label="Refresh agents list"
      >
        <RefreshCw class={['size-4', agentsStore.loading ? 'animate-spin' : '']} />
      </Button>
    </div>
    <div class="mt-4 flex items-center gap-2">
      <div class="relative w-full max-w-md">
        <Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          bind:value={query}
          placeholder="Search agents"
          class="h-9 pl-8 text-sm"
          autocomplete="off"
        />
      </div>
      <span class="text-xs text-muted-foreground">
        {filtered.length} of {agentsStore.list.length}
      </span>
    </div>
  </header>

  <div class="flex-1 overflow-y-auto px-6 py-6">
    {#if agentsStore.loading && agentsStore.list.length === 0}
      <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if agentsStore.error}
      <p class="text-sm text-destructive">Couldn't load: {agentsStore.error}</p>
    {:else if agentsStore.list.length === 0}
      <div class="rounded-lg border border-dashed border-border/60 p-8 text-center">
        <Bot class="mx-auto mb-3 size-8 text-muted-foreground/60" />
        <p class="text-sm text-muted-foreground">
          No agents yet. Drop a markdown file into
          <code class="rounded bg-muted px-1 font-mono">~/.roy/agents/&lt;name&gt;.md</code>
          to populate this catalog.
        </p>
      </div>
    {:else if filtered.length === 0}
      <p class="text-sm text-muted-foreground">No agents match “{query}”.</p>
    {:else}
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {#each filtered as a (a.name)}
          <button
            type="button"
            onclick={() => (selected = a)}
            class="flex h-full flex-col gap-2 rounded-lg border border-border/60 bg-card px-4 py-3 text-left transition-colors hover:border-border hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
          >
            <header class="flex items-baseline justify-between gap-2">
              <h2 class="truncate font-mono text-sm text-foreground">{a.name}</h2>
              <span
                class="flex shrink-0 items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground"
              >
                <ProviderIcon name={agentIcon(a.engine)!} class="size-3" />
                {a.engine}
              </span>
            </header>
            {#if a.description}
              <p class="line-clamp-3 text-xs text-muted-foreground">{a.description}</p>
            {/if}
            {#if a.model}
              <code class="truncate text-[10px] text-muted-foreground/80">{a.model}</code>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<Dialog.Root open={selected !== null} onOpenChange={(o) => (o ? null : (selected = null))}>
  <Dialog.Content class="flex h-[92vh] w-[min(76rem,96vw)] max-w-none flex-col overflow-hidden p-0 sm:max-w-none">
    {#if selected}
      {@const sel = selected}
      <Dialog.Header class="shrink-0 border-b border-border/40 px-6 py-4">
        <Dialog.Title class="flex items-center justify-between gap-3 pr-8">
          <span class="break-all font-mono text-base text-foreground">{sel.name}</span>
          <span
            class="flex shrink-0 items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground"
          >
            <ProviderIcon name={agentIcon(sel.engine)!} class="size-3" />
            {sel.engine}{sel.model ? ` · ${sel.model}` : ''}
          </span>
        </Dialog.Title>
        {#if sel.description}
          <Dialog.Description class="break-words text-xs leading-relaxed">
            {sel.description}
          </Dialog.Description>
        {/if}
      </Dialog.Header>

      <div class="min-h-0 flex-1 overflow-y-auto px-6 py-4">
        <pre
          class="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-foreground">{sel.body}</pre>
      </div>

      <div class="shrink-0 border-t border-border/40 px-6 py-3">
        <Button
          onclick={() => void run(sel)}
          disabled={running !== null}
          class="ml-auto flex"
        >
          <Play class="size-4" />
          {running === sel.name ? 'Spawning…' : 'Run'}
        </Button>
      </div>
    {/if}
  </Dialog.Content>
</Dialog.Root>
```

- [ ] **Step 2: Update the `<AgentsView>` props in App.svelte**

The old view took `onOpenSidebar` and `onOpenBuilder`. The new view takes only `onOpenSession`. Edit `src/App.svelte` — find the `<AgentsView ... />` render block. Replace its props with:

```svelte
<AgentsView onOpenSession={openSession} />
```

(Implementer: the prop signature in the rewritten view above omits `onOpenSidebar` for brevity — if other consumers of AgentsView pass it, keep it as an optional pass-through. Easiest: read the current `<AgentsView ...>` block and remove `onOpenBuilder` while keeping `onOpenSidebar` if present. Add it back to the props if needed.)

- [ ] **Step 3: Verify types**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run check 2>&1 | tail -10`
Expected: errors only from places that still reference the OLD `management.list/run/etc.` or `AgentBuilder*`. We delete those in Task 6 — expected at this stage.

If the only errors come from `App.svelte` and unrelated files we haven't touched yet, proceed. If `AgentsView.svelte` itself has errors, fix them before committing.

- [ ] **Step 4: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/AgentsView.svelte src/App.svelte
git commit -m "feat(agents): read-only catalog mirroring /skills with Run action"
```

---

## Task 6: Frontend — remove builder + DB-backed agents from the SPA

**Files:**
- Modify: `src/lib/ModelPicker.svelte`
- Modify: `src/lib/Composer.svelte`
- Modify: `src/lib/App.svelte`
- Modify: `src/lib/SessionList.svelte`
- Modify: `src/lib/management-client.ts`
- Modify: `src/lib/utils.ts`
- Delete: `src/lib/AgentBuilderView.svelte`
- Delete: `src/lib/agent-builder-store.svelte.ts`
- Delete: `src/lib/management-agents.svelte.ts`

Do this all in one commit to avoid an intermediate state where `App.svelte` still imports a deleted file.

- [ ] **Step 1: ModelPicker.svelte — switch import + rename fields**

In `src/lib/ModelPicker.svelte`:

1. Replace `import { agents } from './management-agents.svelte';` with `import { agentsStore } from './agents.svelte';`.
2. Replace all references to `agents.list`, `agents.loading`, `agents.error`, `agents.refresh()` with `agentsStore.list`, `agentsStore.loading`, `agentsStore.error`, `agentsStore.load()`.
3. In the agents-panel `{#each agents.list as a (a.id)}` loop:
   - The key becomes `(a.name)` (unique per file).
   - The click handler references `a.preset` → change to `a.engine`. References to `a.id` → change to `a.name`.
   - The handler currently does the catalog lookup via `catalogByPreset.get(preset)?.models.find((m) => m.id === a.model)`. Keep that — model id is still optional from the file.
   - `onPickAgent?.(a.id)` → `onPickAgent?.(a.name)`.
4. The `{a.preset}` chip in the rendered row → `{a.engine}`.
5. The `KNOWN_PRESETS.has(a.preset as AgentPreset)` guard → `KNOWN_PRESETS.has(a.engine as AgentPreset)` (and the cast becomes `a.engine as AgentPreset`).

- [ ] **Step 2: Composer.svelte — rename `selectedAgentId` → `selectedAgentName`**

In `src/lib/Composer.svelte`:

1. Replace the `import { agents } from './management-agents.svelte';` with `import { agentsStore } from './agents.svelte';`.
2. Rename `let selectedAgentId = $state<string | undefined>(undefined);` to `let selectedAgentName = $state<string | undefined>(undefined);`.
3. `selectedAgent` `$derived` becomes:
```ts
let selectedAgent = $derived(
  selectedAgentName !== undefined
    ? agentsStore.list.find((a) => a.name === selectedAgentName)
    : undefined,
);
```
4. `selectedAgentLabel` keeps the `⌗ ${name}` formatting, unchanged.
5. `<ModelPicker ... onPickAgent={(name) => { selectedAgentName = name; }} ... />` — rename the callback parameter and target.
6. `onChange={() => { selectedAgentName = undefined; }}` — same rename in the `onChange` handler.
7. On submit, `persona: selectedAgent ? { prompt: selectedAgent.body, name: selectedAgent.name } : undefined` — the field name changed from `prompt` to `body` on the agent object. Update accordingly.
8. The stale-id guard:
```ts
if (selectedAgentName !== undefined && !selectedAgent) {
  selectedAgentName = undefined;
}
```

- [ ] **Step 3: App.svelte — remove builder route**

In `src/App.svelte`:

1. Delete `import AgentBuilderView from './lib/AgentBuilderView.svelte';`.
2. From `type Route`, remove `| { kind: 'builder'; agentId: string; sessionId: string }`.
3. In `parseRoute()`, delete the `const builder = window.location.pathname.match(/^\/agents\/([^/]+)\/?$/);` block and its return.
4. In `pathFor()`, delete the `if (r.kind === 'builder') return ...` branch.
5. In `applyRoute()`, delete the `if (r.kind === 'builder' && !r.sessionId) { ... }` block (the entire builder-resolution path).
6. Delete the `async function openBuilder(existingId?: string) { ... }` function.
7. Wherever `<AgentBuilderView ... />` is rendered (search for it), delete that branch of the route switch.
8. The `<AgentsView ... />` render block — replace `onOpenBuilder={openBuilder}` with `onOpenSession={openSession}` (the function defined further up in the file).
9. Delete the import `lsSet, LS` references that touch `LS.builderSession` (if any local code uses them just to write the builder key — keep the import line if other LS keys are still in use; only delete the calls).

- [ ] **Step 4: SessionList.svelte — drop builder marker**

Search: `grep -n "builder\|TAG_BUILDER\|wrench" src/lib/SessionList.svelte`
For each line that handles the builder-session case, delete the line. Typically a `#if` block testing `session.tags[TAG_BUILDER_AGENT_ID]` and rendering a wrench icon. Delete the icon import and the conditional.

- [ ] **Step 5: management-client.ts — purge dead types and methods**

In `src/lib/management-client.ts`:

1. Delete the entire `Agent` type block (the one with `id, name, slug, preset, model, prompt, ...`).
2. Delete `NewAgent`, `AgentPatch`, `StartBuilderResp`.
3. Delete `export const TAG_BUILDER_AGENT_ID = 'roy-management:builder.agent_id';`.
4. Inside `export const management = { ... }`, delete every property: `list`, `get`, `create`, `update`, `remove`, `run`, `startBuilder`. After deletion, if `management` is empty, delete the whole `export const management` declaration. Other exports (`sessions`, `projects`, `teams`, `scheduler`, `uploads`) stay.

- [ ] **Step 6: utils.ts — drop `LS.builderSession`**

In `src/lib/utils.ts`, find the `LS` const literal. Delete the line:
```ts
  builderSession: (agentId: string) => `roy:builder-session:${agentId}`,
```

Plus the comment above it.

- [ ] **Step 7: Delete the three obsolete files**

```bash
cd /Users/i_strelov/Projects/roy-web
rm src/lib/AgentBuilderView.svelte src/lib/agent-builder-store.svelte.ts src/lib/management-agents.svelte.ts
```

- [ ] **Step 8: Verify types**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run check 2>&1 | tail -15`
Expected: 0 errors (1 pre-existing autofocus warning OK).

If there are errors, they should be limited to: imports of just-deleted files, or refs to removed types. Fix in place — search for the missing symbol via `grep -rn '<symbol>' src/lib/` and either remove the line or thread the new equivalent.

- [ ] **Step 9: Verify build**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run build 2>&1 | tail -5`
Expected: success.

- [ ] **Step 10: Commit**

```bash
cd /Users/i_strelov/Projects/roy-web
git add -A
git commit -m "refactor: delete AgentBuilder, management-agents store, builder route

Frontend side of the file-based agents migration. All consumers of the
old DB-backed agents API are gone; the picker, composer, and /agents
page now read agentsStore (file-backed)."
```

---

## Task 7: Backend — delete old `/agents/*` handlers + roy-agents crate

**Files (roy repo):**
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/src/state.rs`
- Modify: `crates/roy-management/src/lib.rs`
- Modify: `crates/roy-management/Cargo.toml`
- Modify: `crates/roy-cli/src/management_client.rs`
- Modify: `crates/roy-cli/src/management.rs` (only if it has agent subcommands)
- Modify: `crates/roy-cli/Cargo.toml`
- Modify: `Cargo.toml` (workspace root)
- Delete: `crates/roy-agents/` (whole directory)

Now the frontend has stopped calling the old endpoints, so we can drop the backend code.

- [ ] **Step 1: http.rs — rename the new endpoint to `/agents` and delete the old handlers**

Edit `crates/roy-management/src/http.rs`:

1. Delete `use roy_agents::{Agent, AgentUpdate, NewAgent, StoreError};` from the top of the file.
2. Delete the `impl From<StoreError> for ApiError { ... }` block.
3. Delete these route lines from `router()`:
   ```rust
           .route("/agents", get(list_agents).post(create_agent))
           .route("/agents/_builder", post(start_builder))
           .route(
               "/agents/{id}",
               get(get_agent).put(update_agent).delete(delete_agent),
           )
           .route("/agents/{id}/run", post(run_agent))
   ```
4. Rename the new route from `/agents-files` to `/agents`:
   ```rust
           .route("/agents", get(list_agent_files))
   ```
5. Delete the now-dead handler functions: `list_agents`, `get_agent`, `create_agent`, `update_agent`, `delete_agent`, `run_agent`, `start_builder`. Grep for `async fn list_agents` / etc. and delete each function in full. Also delete any helper structs they used (e.g., `BuilderReq` for the start_builder endpoint).
6. Delete the test functions that hit the deleted routes. Grep `cargo test --no-run -p roy-management 2>&1` after pruning to verify what's still referenced. Tests to delete:
   - The CRUD round-trip tests (look for `Request::post("/agents")`, `Request::get("/agents/`, etc.).
   - The builder tests (look for `Request::post("/agents/_builder")`).
   - Keep the `list_agent_files_returns_parsed_entries` test from Task 3 (now hitting `/agents` instead of `/agents-files` — update its URL).
7. Update the frontend client too: edit `src/lib/agents.svelte.ts` to use `/management/agents` (drop the `-files` suffix). One-line change.

- [ ] **Step 2: state.rs — drop the Store field**

Edit `crates/roy-management/src/state.rs`:
1. Delete `use roy_agents::Store;`.
2. Delete the `pub store: Store,` field.

- [ ] **Step 3: lib.rs — drop the Store init**

Edit `crates/roy-management/src/lib.rs`. Find the AppState construction (around line 77) and delete:
```rust
store: roy_agents::Store::new(pool.clone()),
```

Also delete any other `roy_agents::` references in this file.

- [ ] **Step 4: Cargo.toml (roy-management) — drop the dep**

Edit `crates/roy-management/Cargo.toml`. Find `roy-agents = { path = "../roy-agents" }` and delete the line.

- [ ] **Step 5: roy-cli/management_client.rs — delete agent helpers**

Edit `crates/roy-cli/src/management_client.rs`:
1. Delete the `Agent`, `AgentUpdate`, `NewAgent` re-exports / types.
2. Delete the methods on whatever client struct lives there: `list_agents`, `get_agent`, `create_agent`, `update_agent`, `delete_agent`, `run_agent`.
3. If the file becomes thin (just project/session helpers), leave the rest alone.

- [ ] **Step 6: roy-cli — remove agent subcommands**

Grep: `cd /Users/i_strelov/Projects/roy && grep -n "Agents\|agents::\|AgentsList\|ManagementCmd::Agent" crates/roy-cli/src/management.rs crates/roy-cli/src/main.rs 2>/dev/null`

For each subcommand in `roy-cli` that calls a deleted helper (`agents list`, `agents create`, `agents update`, `agents delete`, `agents run`, possibly `agents _builder`), delete the variant from the clap enum + its handler arm. Leave unrelated subcommands alone.

If the file uses `roy_agents::default_db_path()` anywhere besides `auth.rs` (Task 1 handled that), redirect to `roy_management::db::default_db_path()` as well.

- [ ] **Step 7: Cargo.toml (roy-cli) — drop the dep**

Edit `crates/roy-cli/Cargo.toml`. Delete `roy-agents = { path = "../roy-agents" }` if present.

- [ ] **Step 8: Workspace root Cargo.toml — drop crate from members**

Edit `/Users/i_strelov/Projects/roy/Cargo.toml`. Find the `members = [...]` array and delete the `"crates/roy-agents"` entry.

- [ ] **Step 9: Delete the crate**

```bash
cd /Users/i_strelov/Projects/roy
rm -rf crates/roy-agents
```

- [ ] **Step 10: Add the drop-table migration**

Create `crates/roy-management/migrations/sqlite/0006_drop_legacy_agents.sql`:

```sql
-- The roy-agents crate has been removed; agents now live as files in
-- ~/.roy/agents/<name>.md. Drop the legacy table on existing deployments
-- so the DB doesn't carry around orphaned rows. Fresh deployments never
-- had the table (those migrations went away with the crate).
DROP TABLE IF EXISTS agents;
```

(File number `0006` continues the existing roy-management migration sequence — confirm the next number by `ls crates/roy-management/migrations/sqlite/`.)

- [ ] **Step 11: Build the whole workspace**

Run: `cd /Users/i_strelov/Projects/roy && cargo build 2>&1 | tail -20`
Expected: success. Fix any leftover references the implementer missed.

- [ ] **Step 12: Run all tests**

Run: `cd /Users/i_strelov/Projects/roy && cargo test 2>&1 | tail -25`
Expected: all tests pass. If a test references deleted types, delete the test.

- [ ] **Step 13: Re-run the frontend type-check (URL change)**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run check 2>&1 | tail -5`
Expected: 0 errors. The agents.svelte.ts URL was updated in Step 1.

- [ ] **Step 14: Commit the backend changes (roy repo)**

```bash
cd /Users/i_strelov/Projects/roy
git add -A
git commit -m "refactor: drop roy-agents crate, agents are now files

Deletes the DB-backed agents store and the chat-driven builder bot.
The new file-based scanner under crates/roy-management/src/agents.rs
plus GET /management/agents replaces the old /agents/* CRUD surface.
Adds a DROP TABLE agents migration so existing deployments end up
clean."
```

- [ ] **Step 15: Commit the frontend URL fix (roy-web)**

```bash
cd /Users/i_strelov/Projects/roy-web
git add src/lib/agents.svelte.ts
git commit -m "feat(agents): consume permanent /management/agents endpoint"
```

---

## Task 8: Docker rebuild + restart

**Files:** none (commands only)

- [ ] **Step 1: Rebuild the daemon image**

Run: `cd /Users/i_strelov/Projects/roy-docker && docker compose build roy-daemon roy-management roy-gateway 2>&1 | tail -8`
Expected: successful build. The Rust binary now contains the new endpoint.

- [ ] **Step 2: Rebuild the web image**

Run: `cd /Users/i_strelov/Projects/roy-docker && docker compose build roy-web 2>&1 | tail -5`
Expected: success.

- [ ] **Step 3: Recreate containers**

Run: `cd /Users/i_strelov/Projects/roy-docker && docker compose up -d 2>&1 | tail -10`
Expected: all services Started or running.

- [ ] **Step 4: Sanity-check the new endpoint**

Run: `curl -s -b /tmp/roy-cookies.txt http://localhost:8079/agents | head -100`
(Or via the gateway: `http://localhost:8080/management/agents` from the browser, with an authenticated session.)

If empty array — expected (no files exist yet).

- [ ] **Step 5: Create a sample agent file**

```bash
mkdir -p ~/.roy/agents
cat > ~/.roy/agents/pirate.md <<'EOF'
---
name: pirate
description: Pirate-themed coding assistant
engine: codex
---

You are a pirate. End every reply with "Arr."
EOF
```

- [ ] **Step 6: Manual walkthrough**

Open `http://localhost:8080/agents` in the browser. Expected:
1. The `pirate` card appears (allow up to 30 s for cache TTL after writing the file, or call refresh).
2. Click the card — modal opens showing the body.
3. Click Run — a new chat session opens; the first assistant turn (after you type any message) should reply in pirate voice.

Also open a new chat (`/`), open the picker, click the 🤖 Agents tab. The `pirate` agent should appear; clicking it sets the pill to `⌗ pirate`. Submit a message — same persona behavior.

If anything is wrong (no agents, persona not applied, etc.) — capture the failing step and fix.

---

## Self-review

**Spec coverage:**

- File format (`~/.roy/agents/<name>.md`, frontmatter with `engine` marker, optional `model`): Task 2 (parser + scanner) + Task 5 (Run flow consumes those fields).
- Backend new `GET /management/agents` (body inline): Task 3 (temp path), Task 7 step 1 (final path).
- Delete `roy-agents` crate, relocate `default_db_path`: Task 1 (relocate) + Task 7 (delete).
- `DROP TABLE agents` migration: Task 7 step 10.
- Frontend `agentsStore`: Task 4.
- `AgentsView` mirroring `SkillsView`: Task 5.
- ModelPicker Agents tab adaptation: Task 6 step 1.
- Composer `selectedAgent` adaptation: Task 6 step 2.
- Deletions (AgentBuilderView, agent-builder-store, management-agents.svelte, builder route, builder LS key, builder-session marker, dead types in management-client): Task 6 steps 3-7 + Task 7 (backend handlers).
- Docker rebuild: Task 8.

**Placeholder scan:** No "TBD" or "implement later" remain. Every step shows the change. The `_home`/`test_router` helper name in Task 3's test mention is described as "adapt to actual helper name" — flagged as a real lookup, not a placeholder.

**Type consistency:** `Agent` (new shape) has `engine: AgentPreset, body: string, name: string`. Used identically in Tasks 4, 5, 6. `selectedAgentName` (Task 6 step 2) replaces the previous `selectedAgentId` from the picker plan — pattern unchanged otherwise. `agentsStore.load()` is the only API consumers call, used in Tasks 4, 5, 6.
