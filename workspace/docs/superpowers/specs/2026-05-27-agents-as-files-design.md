# Agents as files (drop the DB)

Status: approved (2026-05-27)
Spans two repos:
- `/Users/i_strelov/Projects/roy` (Rust: roy-management, roy-cli, roy-agents crate)
- `/Users/i_strelov/Projects/roy-web` (Svelte SPA)

## Problem

Today an Agent is a DB row in `roy-agents` (SQLite). The web app edits it via a chat-driven "Agent Builder" — a spawned ACP session with a hand-crafted system prompt that the LLM uses to drive an interactive CRUD UX. Creating an agent means starting a chat, talking to the bot, watching it call CLI commands to persist the row. That's heavy machinery for what is, conceptually, "save a name + a system prompt + which engine/model to use."

## Goals

- Make agents first-class filesystem artifacts, like skills already are.
- One file per agent, hand-editable, version-controllable.
- `/agents` page mirrors `/skills` page (read-only catalog of cards → modal with body).
- Spawning an agent uses the same `system_prompt` / `agent_name` persona path that the Composer already wires (Tasks 8/9 of the picker plan).
- Delete the chat-driven builder, the DB table, the CRUD endpoints, and the `roy-agents` crate.

## Non-goals

- Web-side creation of agent files. Users drop `.md` files in `~/.roy/agents/` by hand.
- Migrating existing DB rows into files. Users rewrite the few they actually need.
- Touching the daemon's `ServerEvent::AgentsList` (engine catalog) — unrelated concept that happens to share the word.
- Touching `roy-scheduler` agents (separate internal store under `crate::store::agents`).

## File format

Path: `~/.roy/agents/<name>.md`

```yaml
---
name: pirate-coder
description: Pirate-themed coding assistant
engine: codex                # claude | gemini | opencode | codex | pi
model: gpt-5.4               # optional, falls back to engine default
---

You are a pirate. End every reply with "Arr."
Help the user code while staying in character.
```

- One `.md` file per agent at the top level of `~/.roy/agents/`. No nested `<name>/AGENT.md` — matches Claude Code subagents.
- The presence of an `engine` field is what marks a file as an agent. Files without `engine` are silently ignored (no implicit fallback to a default engine).
- `model` is optional; missing → engine's default model at run time.
- Body (markdown after the second `---`) becomes the spawned session's `system_prompt`.
- `name` in frontmatter must match the filename stem; mismatches are tolerated but the filename wins for routing.

Filename safety: `[a-z0-9-_]{1,64}` matching the existing `is_safe_skill_name` rule.

## Backend changes (roy repo)

### Delete the crate

Remove `crates/roy-agents/` entirely. Three things depend on it:

1. `roy-management` consumes `Store`, `Agent`, `NewAgent`, `AgentUpdate`, slug helpers, and `default_db_path` / `open`.
2. `roy-cli` consumes only `default_db_path` from `auth.rs`.
3. `roy-agents/migrations/sqlite/` holds the table schema (3 files).

Migration strategy: add a new migration **in `roy-management`** that does `DROP TABLE IF EXISTS agents;`. Move the existing `0001_agents.sql` / `0002_builder_seed.sql` / `0003_builder_seed_v2.sql` migration files to `roy-management/migrations/sqlite/` so existing deployments still see the original create steps (sqlx-migrate fails on missing prior migrations). The new drop runs last.

### Relocate `default_db_path`

The shared SQLite path now lives in `roy-management/src/db.rs`. Export:

```rust
pub fn default_db_path() -> PathBuf {
    // existing implementation, unchanged
}
pub async fn open(...) -> Result<SqlitePool, ...> { /* unchanged */ }
```

`roy-cli/src/auth.rs:50` switches from `roy_agents::default_db_path()` to `roy_management::db::default_db_path()`.

### Replace the agents API

In `roy-management`:

1. Delete handlers: `GET /agents`, `GET /agents/{id}`, `POST /agents`, `PUT /agents/{id}`, `DELETE /agents/{id}`, `POST /agents/{id}/run`, `POST /agents/_builder`.
2. Delete the in-process state holding `roy_agents::Store`.
3. New module `crates/roy-management/src/agents.rs`:

```rust
pub fn roy_agents_dir(home: &Path) -> PathBuf { home.join(".roy/agents") }

#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    pub name: String,
    pub description: String,
    pub engine: String,        // freeform string; web validates
    pub model: Option<String>,
    pub body: String,
}

pub async fn list_agents_from(home: &Path) -> Vec<AgentFile>;
```

Scan `*.md` files at the top level of `~/.roy/agents/`. Reuse the `parse_skill_md` logic but with extended frontmatter parsing (also pull `engine` + `model`). Skip files without `engine`.

4. New HTTP handler `GET /management/agents` → `Vec<AgentFile>` (JSON). Body included inline; files are small and the list is short, so one round-trip is fine.
5. Cache: same 30-second TTL pattern used by `CommandsCache`. Separate cache instance — different scan directory.

### Delete roy-cli agent subcommands

`roy-cli/src/management_client.rs` has `list/get/create/update/delete/run` helpers for `/agents/*`. Delete them along with whatever CLI subcommands invoke them. The CLI did not have a "create agent from file" flow — users now just edit files directly.

## Frontend changes (roy-web)

### New store

`src/lib/agents.svelte.ts`:

```ts
export type Agent = {
  name: string;
  description: string;
  engine: AgentPreset;       // matches the API field name
  model?: string;
  body: string;
};

class AgentsState {
  list = $state<Agent[]>([]);
  loading = $state(false);
  loaded = $state(false);
  error = $state<string | null>(null);

  async load(force = false): Promise<void> { /* GET /management/agents, mirror commands.svelte.ts */ }
}

export const agentsStore = new AgentsState();
```

Hard-coded validation: drop entries whose `engine` isn't in the `KNOWN_PRESETS` set (from `wire.ts`). Log a console warning so the user knows their file has an unknown engine.

### Rewrite `AgentsView.svelte`

Mirror `SkillsView.svelte`:

- Header with title, description, refresh button, search box.
- Grid of card buttons (engine + model chips, name, description preview).
- Click → `Dialog.Root` modal with body rendered as `<pre>` (read-only, same as Skills modal).
- Modal footer: "Run" button → spawns a session with the agent's persona, navigates to the new chat.

Run flow:

```ts
async function run(a: Agent) {
  const model = a.model ?? defaultModelFor(enginesConfig.engines, a.engine)?.id;
  if (!model) return;  // engine not in catalog — surface an error
  const sessionId = await app.createSession({
    agent: a.engine,
    model,
    persona: { prompt: a.body, name: a.name },
  });
  onOpenSession?.(sessionId);
}
```

No first prompt (matches today's `agents.run(id)` semantics — the user types their first message in the new chat).

### Adapt `ModelPicker.svelte` Agents tab

Replace the `management-agents` store reference with `agentsStore`. The tab's row click already calls `pickModel` + `onPickAgent`. After the swap:

- `a.preset` becomes `a.engine` — rename the local destructure.
- `a.id` (used by `selectedAgentId` mechanism) becomes `a.name` — names are unique across files. Or change `selectedAgentId` to `selectedAgentName` for clarity.
- Lazy-fetch effect: same shape, calls `agentsStore.load()` instead of `agents.refresh()`.

### Adapt `Composer.svelte`

- `selectedAgent` `$derived` reads from `agentsStore.list` and matches by `name`.
- `selectedAgentLabel` formatting: keep the existing `⌗ ${name}`. The picker rail uses a Lucide `Bot` SVG icon; the pill is a `font-mono` text span where a Lucide component would look out of place. Unicode glyph wins.
- The `createSession({ persona: { prompt, name } })` call already accepts the right shape (T8).

### Deletions

Remove from `roy-web`:

- `src/lib/AgentBuilderView.svelte`
- `src/lib/agent-builder-store.svelte.ts`
- `src/lib/management-agents.svelte.ts` (replaced by `agents.svelte.ts`)
- From `management-client.ts`: `Agent`, `NewAgent`, `AgentPatch`, `StartBuilderResp`, `TAG_BUILDER_AGENT_ID`, `management.{list,get,create,update,remove,run,startBuilder}`. Keep `sessions.create` (still used).
- From `utils.ts:LS`: `builderSession` key (and helper).
- From `App.svelte`: import + route for `AgentBuilderView`, `onOpenBuilder` prop threading, builder-session navigation.
- From `SessionList.svelte`: builder-session marker / wrench icon (if any).
- From `ChatView.svelte`: any builder-specific UI affordances (if any).

### Not changed

- The picker's Composer-only Agents tab visibility (`showAgentsTab` from the previous commit) — keeps working since it just reads `agentsStore.list.length > 0`.
- Persona-injection plumbing in `state.svelte.ts:createSession` — already accepts `persona?: { prompt; name }`.
- ChatView's locked-agent picker — agent rail still hidden under `lockAgent`.

## Failure modes

- **File with unknown `engine`**: web filters it out and console-warns. Backend includes it in the response (raw string) — web is the gatekeeper.
- **File without `engine`**: backend filter drops it. Never reaches web.
- **File with `model` not in engine's catalog**: web falls back to engine default on run. Picker shows the configured `model` value in the chip even if it's stale (just visual).
- **Existing DB rows on upgrade**: the new `DROP TABLE` migration removes them. Users who relied on them rebuild as files. There's no migration helper; the user said this is acceptable.
- **`~/.roy/agents/` doesn't exist**: backend returns empty list. Web shows the empty state ("No agents yet. Drop a markdown file into ~/.roy/agents/&lt;name&gt;.md to populate this catalog.").

## Cross-cutting

- Web type names: rename `Agent` → keep, the new file-shape `Agent` from `agents.svelte.ts` replaces the deleted DB-shape `Agent` from `management-client.ts`. Same name, different shape — but the old one is deleted entirely so no clash.
- The word "agent" remains ambiguous (preset/engine vs persona). Spec uses "engine" for the preset side, "agent" for the persona file. Picker visuals (the 🤖 icon) make the distinction visual.

## Open questions

None. Implementation details (exact module boundaries in `roy-management/src/`, migration ordering) handed off to the plan.
