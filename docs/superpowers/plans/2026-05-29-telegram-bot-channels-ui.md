# Telegram Bot Channels UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `/channels` page to the roy web frontend to add, list, delete, and enable/disable Telegram support bots without curl.

**Architecture:** A bot is two backend objects — a `telegram_bot` connection (token) and a `channel_binding` (agent + session strategy). The frontend orchestrates both creates with rollback. Two small backend additions fill real gaps: a `PATCH /channel-bindings/{id}` enable/disable endpoint, and exposing the agent file `slug` on `GET /agents` (the binding stores `agent_slug` = file stem, but the list only returned the frontmatter `name`).

**Tech Stack:** Rust (axum, sqlx, roy-management), TypeScript + Svelte 5 (runes) + Vite + Tailwind 4 + bits-ui (workspace/).

**Spec:** `docs/superpowers/specs/2026-05-29-telegram-bot-channels-ui-design.md`

**Working branch:** `feat/telegram-bot-channels-ui` (already created; the spec commit is on it).

---

## File Structure

**Backend (`crates/roy-management/src/`):**
- `agents.rs` — add `slug` field to `AgentFile`, populate from the file stem in `list_dir`.
- `channel_bindings.rs` — add `UpdateChannelBinding` body, `Store::set_enabled`, `update_handler`, and a `.patch(...)` route.

**Frontend (`workspace/src/`):**
- `lib/management-client.ts` — widen `Connection.kind`/`config`, add `slug` to `WireAgent`, add `channelBindings` API namespace + `ChannelBinding`/`NewChannelBinding`/`SessionStrategy` types.
- `lib/channels.svelte.ts` — new `LoadableStore<ChannelBinding>` subclass with `addBot`/`setEnabled`/`removeBot` orchestration.
- `lib/ChannelsView.svelte` — the page (list + add button).
- `lib/AddBotDialog.svelte` — the create form (token, agent picker, strategy, idle timeout, allowlist).
- `lib/App.svelte` + `lib/SessionList.svelte` — wire the `/channels` route and sidebar nav.

**Verification commands:**
- Backend: `cargo test -p roy-management`, `cargo build --workspace --all-targets`, `cargo fmt --all -- --check`.
- Frontend: `cd workspace && npm run check && npm run build`.

---

## Task 1: Expose agent `slug` on the agents list (backend)

**Why:** The channel-binding create handler validates `agent_slug` via `read_agent_persona(dir, slug)` where `slug` is the `.md` file stem. But `GET /agents` returns `name` = frontmatter `name` (falling back to stem). When they differ, the UI picker would send the wrong value and the bind would 400. Expose the stem as `slug`.

**Files:**
- Modify: `crates/roy-management/src/agents.rs:29-37` (struct), `:159-166` (`list_dir` push)
- Test: `crates/roy-management/src/agents.rs` `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add this test inside `mod tests` (after `lists_agents_in_alphabetical_order`):

```rust
    #[tokio::test]
    async fn exposes_file_stem_as_slug() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("workspace/users/u1/.roy/agents");
        // Frontmatter name deliberately differs from the file stem.
        write(
            &dir,
            "support-l1.md",
            "---\nname: Support L1\ndescription: d\nharness: claude\n---\nbody\n",
        );
        let list = list_all_agents(
            &home.path().join("builtin"),
            &home.path().join("workspace"),
            "u1",
            &[],
        )
        .await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].slug, "support-l1");
        assert_eq!(list[0].name, "Support L1");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-management exposes_file_stem_as_slug`
Expected: FAIL to compile — `no field 'slug' on type 'AgentFile'`.

- [ ] **Step 3: Add the `slug` field to `AgentFile`**

In `crates/roy-management/src/agents.rs`, change the struct (currently lines 29-37):

```rust
#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    /// File stem (`<slug>.md`). Stable id used by channel bindings.
    pub slug: String,
    pub name: String,
    pub description: String,
    pub harness: String,
    pub model: Option<String>,
    pub body: String,
    pub scope: AgentScope,
}
```

- [ ] **Step 4: Populate `slug` in `list_dir`**

In `list_dir` (currently lines 159-166), the `stem` is moved into `name` via `unwrap_or(stem)`. Clone it for the slug. Replace the `out.push(AgentFile { ... })` block with:

```rust
        out.push(AgentFile {
            slug: stem.clone(),
            name: parsed.name.unwrap_or(stem),
            description: parsed.description.unwrap_or_default(),
            harness,
            model: parsed.model,
            body: parsed.body,
            scope: scope.clone(),
        });
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p roy-management exposes_file_stem_as_slug`
Expected: PASS.

- [ ] **Step 6: Run the full crate tests + fmt**

Run: `cargo test -p roy-management && cargo fmt --all -- --check`
Expected: all pass (existing agent tests still green — `name` assertions unchanged).

- [ ] **Step 7: Commit**

```bash
git add crates/roy-management/src/agents.rs
git commit -m "feat(management): expose agent file slug on GET /agents

The channel-binding validator resolves agents by file stem, but the list
only returned the frontmatter name. Add slug so the web picker can bind
the correct agent."
```

---

## Task 2: `PATCH /channel-bindings/{id}` enable/disable (backend)

**Why:** The UI needs an enable/disable toggle. Today the router has only GET/POST/GET/DELETE; `enabled` is inserted `true` and only read. Add a `set_enabled` store method, a `PATCH` handler, and a unit test.

**Files:**
- Modify: `crates/roy-management/src/channel_bindings.rs` — add `UpdateChannelBinding` (near `NewChannelBinding`, ~line 41), `Store::set_enabled` (after `delete`, ~line 194), `update_handler` (after `delete_handler`, ~line 390), and the `.patch(...)` route (~line 357).
- Test: same file's `#[cfg(test)] mod tests`.

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` (after `one_bot_one_binding`):

```rust
    #[tokio::test]
    async fn toggle_enabled() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let conn_id = make_conn(&pool, &user.id).await;
        let store = Store::new(pool.clone());
        let b = store
            .create(
                &user.id,
                &NewChannelBinding {
                    connection_id: conn_id,
                    agent_slug: "support-l1".into(),
                    agent_scope: "user".into(),
                    session_strategy: "per_sender_sticky".into(),
                    idle_timeout_secs: Some(3600),
                    allowed_user_ids: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(store.list_enabled_telegram().await.unwrap().len(), 1);

        let off = store.set_enabled(&user.id, &b.id, false).await.unwrap();
        assert!(!off.enabled);
        assert!(store.list_enabled_telegram().await.unwrap().is_empty());

        let on = store.set_enabled(&user.id, &b.id, true).await.unwrap();
        assert!(on.enabled);
        assert_eq!(store.list_enabled_telegram().await.unwrap().len(), 1);

        let missing = store.set_enabled(&user.id, "nope", true).await.unwrap_err();
        assert!(matches!(missing, StoreError::NotFound(_)));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-management toggle_enabled`
Expected: FAIL to compile — `no method named 'set_enabled'`.

- [ ] **Step 3: Add `Store::set_enabled`**

In `impl Store`, after the `delete` method (currently ends ~line 194), add:

```rust
    /// Flip the `enabled` flag for one binding. Bumps `updated_at`.
    /// `NotFound` if the binding doesn't exist for this owner.
    pub async fn set_enabled(
        &self,
        owner_id: &str,
        id: &str,
        enabled: bool,
    ) -> Result<ChannelBinding, StoreError> {
        let now = Utc::now().timestamp();
        let res = sqlx::query(
            "UPDATE channel_bindings SET enabled = ?, updated_at = ? \
             WHERE owner_id = ? AND id = ?",
        )
        .bind(enabled as i64)
        .bind(now)
        .bind(owner_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        self.get(owner_id, id).await
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p roy-management toggle_enabled`
Expected: PASS.

- [ ] **Step 5: Add the `UpdateChannelBinding` request body**

After the `NewChannelBinding` struct + `default_strategy` fn (~line 45), add:

```rust
/// Request body for `PATCH /channel-bindings/{id}`. Enable/disable only.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateChannelBinding {
    pub enabled: bool,
}
```

- [ ] **Step 6: Add the `update_handler` and wire the route**

After `delete_handler` (~line 390) add:

```rust
async fn update_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<UpdateChannelBinding>,
) -> Result<Json<ChannelBinding>, ApiError> {
    Ok(Json(
        s.channel_bindings.set_enabled(&uid, &id, body.enabled).await?,
    ))
}
```

Then extend the `/channel-bindings/{id}` route (currently `get(get_handler).delete(delete_handler)`) to:

```rust
        .route(
            "/channel-bindings/{id}",
            get(get_handler).delete(delete_handler).patch(update_handler),
        )
```

(`.patch` is a method on the chained `MethodRouter`; no new import needed.)

- [ ] **Step 7: Build, test, fmt**

Run: `cargo test -p roy-management && cargo build --workspace --all-targets && cargo fmt --all -- --check`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/roy-management/src/channel_bindings.rs
git commit -m "feat(management): PATCH /channel-bindings/{id} to toggle enabled

Adds Store::set_enabled + a PATCH handler so the web UI can enable/disable
a Telegram bot without deleting its binding."
```

---

## Task 3: management-client types + channelBindings API (frontend)

**Files:**
- Modify: `workspace/src/lib/management-client.ts`

- [ ] **Step 1: Widen `Connection` to allow telegram_bot**

Replace the `Connection` type (currently lines 119-131) and `NewConnectionCustom` (lines 168-174) so telegram connections typecheck. New `Connection`:

```ts
/** Mirrors `roy_management::connections::Connection`. `config` is
 *  kind-specific (mcp_stdio → command/args/env; telegram_bot → {}). No UI
 *  reads `config` off a fetched connection, so it's left as an open record. */
export type Connection = {
  id: string;
  owner_id: string;
  name: string;
  slug: string;
  kind: 'mcp_stdio' | 'telegram_bot';
  config: Record<string, unknown>;
  secrets: Record<string, string> | null;
  description: string | null;
  created_at: number;
  updated_at: number;
  provider_id: string | null;
};
```

And widen the custom create body (lines 168-174):

```ts
/** Free-form POST body — used by the custom-MCP dialog and the Telegram
 *  bot create flow. */
export type NewConnectionCustom = {
  name: string;
  kind: 'mcp_stdio' | 'telegram_bot';
  config: Record<string, unknown>;
  secrets?: Record<string, string> | null;
  description?: string | null;
};
```

- [ ] **Step 2: Add `slug` to `WireAgent`**

Replace `WireAgent` (currently lines 245-252) with:

```ts
/** Raw `/management/agents` row. `slug` is the `.md` file stem (stable id
 *  used by channel bindings); `name` is the frontmatter display name. */
export type WireAgent = {
  slug: string;
  name: string;
  description: string;
  harness: string;
  model?: string | null;
  body: string;
  scope: { kind: string; team_id?: string };
};
```

- [ ] **Step 3: Add channel-binding types + API namespace**

After the `connections` const block (ends ~line 208), add:

```ts
export type SessionStrategy = 'ephemeral' | 'persistent_one' | 'per_sender_sticky';

/** Mirrors `roy_management::channel_bindings::ChannelBinding`. */
export type ChannelBinding = {
  id: string;
  owner_id: string;
  channel_kind: string; // "telegram"
  connection_id: string;
  agent_slug: string;
  agent_scope: string; // "user" | "team:<team_id>"
  session_strategy: SessionStrategy;
  idle_timeout_secs: number | null;
  allowed_user_ids: number[];
  enabled: boolean;
  created_at: number;
  updated_at: number;
};

/** Body for `POST /channel-bindings`. */
export type NewChannelBinding = {
  connection_id: string;
  agent_slug: string;
  agent_scope: string;
  session_strategy: SessionStrategy;
  idle_timeout_secs?: number;
  allowed_user_ids?: number[];
};

export const channelBindings = {
  list: () => request<ChannelBinding[]>('/channel-bindings'),
  create: (body: NewChannelBinding) =>
    request<ChannelBinding>('/channel-bindings', {
      method: 'POST',
      body: JSON.stringify(body),
      expectStatus: 201,
    }),
  remove: (id: string) =>
    request<void>(`/channel-bindings/${encodeURIComponent(id)}`, {
      method: 'DELETE',
      expectStatus: 204,
    }),
  setEnabled: (id: string, enabled: boolean) =>
    request<ChannelBinding>(`/channel-bindings/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      body: JSON.stringify({ enabled }),
    }),
};
```

- [ ] **Step 4: Typecheck**

Run: `cd workspace && npm run check`
Expected: no new type errors. (Existing MCP code does not read `Connection.config.command`, so widening `config` is safe.)

- [ ] **Step 5: Commit**

```bash
git add workspace/src/lib/management-client.ts
git commit -m "feat(web): channelBindings API client + telegram_bot connection types"
```

---

## Task 4: channels store (frontend)

**Files:**
- Create: `workspace/src/lib/channels.svelte.ts`

- [ ] **Step 1: Create the store**

Create `workspace/src/lib/channels.svelte.ts`:

```ts
// Client-side store for the user's Telegram bot channel bindings.
// A "bot" is two backend objects: a telegram_bot connection (token) and a
// channel binding (agent + session strategy). addBot orchestrates both and
// rolls back the connection if the bind fails so no orphan token is left.

import {
  channelBindings as api,
  connections as connApi,
  type ChannelBinding,
  type SessionStrategy,
} from './management-client';
import { LoadableStore } from './list-store.svelte';

export type NewBotInput = {
  botName: string;
  botToken: string;
  agentSlug: string;
  agentScope: string;
  sessionStrategy: SessionStrategy;
  idleTimeoutSecs?: number;
  allowedUserIds: number[];
};

class ChannelsState extends LoadableStore<ChannelBinding> {
  async load(force = false): Promise<void> {
    await this.run(() => api.list(), force);
  }

  /// Create the telegram_bot connection, then bind it to the agent. If the
  /// bind fails, delete the just-created connection so a failed attempt
  /// doesn't strand a bot token in the DB.
  async addBot(input: NewBotInput): Promise<ChannelBinding> {
    const conn = await connApi.create({
      name: input.botName,
      kind: 'telegram_bot',
      config: {},
      secrets: { bot_token: input.botToken },
    });
    try {
      const binding = await api.create({
        connection_id: conn.id,
        agent_slug: input.agentSlug,
        agent_scope: input.agentScope,
        session_strategy: input.sessionStrategy,
        idle_timeout_secs: input.idleTimeoutSecs,
        allowed_user_ids: input.allowedUserIds,
      });
      this.list = [binding, ...this.list];
      return binding;
    } catch (e) {
      // Best-effort rollback. Swallow its error so the user sees the real
      // bind failure, not a secondary cleanup error.
      try {
        await connApi.remove(conn.id);
      } catch {
        /* leave the orphan; surface the original error below */
      }
      throw e;
    }
  }

  async setEnabled(id: string, enabled: boolean): Promise<void> {
    const updated = await api.setEnabled(id, enabled);
    this.list = this.list.map((b) => (b.id === id ? updated : b));
  }

  /// Delete the binding, then its connection (so the bot token is gone too).
  async removeBot(binding: ChannelBinding): Promise<void> {
    await api.remove(binding.id);
    try {
      await connApi.remove(binding.connection_id);
    } catch {
      /* binding already gone; an orphan connection is harmless and re-deletable */
    }
    this.list = this.list.filter((b) => b.id !== binding.id);
  }
}

export const channelsStore = new ChannelsState();
export type { ChannelBinding } from './management-client';
```

- [ ] **Step 2: Typecheck**

Run: `cd workspace && npm run check`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add workspace/src/lib/channels.svelte.ts
git commit -m "feat(web): channels store with add/remove/toggle bot orchestration"
```

---

## Task 5: AddBotDialog component (frontend)

**Files:**
- Create: `workspace/src/lib/AddBotDialog.svelte`

This dialog fetches the agent list on open, lets the user pick an agent, a
session strategy (default `per_sender_sticky`), an idle timeout (shown only for
sticky), and an optional allowlist of numeric Telegram user IDs.

- [ ] **Step 1: Create the dialog**

Create `workspace/src/lib/AddBotDialog.svelte`:

```svelte
<script lang="ts">
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import * as Dialog from '$lib/components/ui/dialog';
  import * as Select from '$lib/components/ui/select';
  import { channelsStore } from './channels.svelte';
  import { agents as agentsApi, type WireAgent, type SessionStrategy } from './management-client';
  import { errMsg } from './utils';

  let {
    open = $bindable(false),
    onAdded,
  }: {
    open?: boolean;
    onAdded?: () => void;
  } = $props();

  let botName = $state('');
  let botToken = $state('');
  // Encoded "agentScope::slug" so the value carries both the slug and the
  // scope the binding needs (two agents could share a slug across scopes).
  let agentValue = $state('');
  let strategy = $state<SessionStrategy>('per_sender_sticky');
  let idleMinutes = $state(60);
  let allowlistRaw = $state('');
  let agentList = $state<WireAgent[]>([]);
  let submitting = $state(false);
  let error = $state<string | null>(null);

  function scopeString(a: WireAgent): string {
    return a.scope.kind === 'team' && a.scope.team_id ? `team:${a.scope.team_id}` : 'user';
  }
  function encode(a: WireAgent): string {
    return `${scopeString(a)}::${a.slug}`;
  }

  const STRATEGIES: { value: SessionStrategy; label: string }[] = [
    { value: 'per_sender_sticky', label: 'Per sender (sticky)' },
    { value: 'persistent_one', label: 'One shared session' },
    { value: 'ephemeral', label: 'Ephemeral (fresh each message)' },
  ];

  const selectedAgentLabel = $derived(
    agentList.find((a) => encode(a) === agentValue)?.name ?? 'Select an agent',
  );
  const strategyLabel = $derived(
    STRATEGIES.find((s) => s.value === strategy)?.label ?? '',
  );

  // Fresh form + agent fetch on each open.
  $effect(() => {
    if (open) {
      botName = '';
      botToken = '';
      agentValue = '';
      strategy = 'per_sender_sticky';
      idleMinutes = 60;
      allowlistRaw = '';
      error = null;
      void agentsApi.list().then((a) => (agentList = a)).catch((e) => (error = errMsg(e)));
    }
  });

  /// Parse "111, 222 333" → [111, 222, 333]. Returns null on any non-numeric
  /// token so we can reject the form instead of silently dropping it.
  function parseAllowlist(raw: string): number[] | null {
    const out: number[] = [];
    for (const tok of raw.split(/[\s,]+/).filter(Boolean)) {
      const n = Number(tok);
      if (!Number.isInteger(n) || n <= 0) return null;
      out.push(n);
    }
    return out;
  }

  async function submit() {
    if (submitting) return;
    if (!botName.trim()) return (error = 'Bot name is required');
    if (!botToken.trim()) return (error = 'Bot token is required');
    if (!agentValue) return (error = 'Pick an agent');
    if (strategy === 'per_sender_sticky' && (!idleMinutes || idleMinutes <= 0)) {
      return (error = 'Idle timeout must be a positive number of minutes');
    }
    const allowed = parseAllowlist(allowlistRaw);
    if (allowed === null) return (error = 'Allowlist must be space/comma-separated numeric user IDs');

    const sep = agentValue.indexOf('::');
    const agentScope = agentValue.slice(0, sep);
    const agentSlug = agentValue.slice(sep + 2);

    submitting = true;
    error = null;
    try {
      await channelsStore.addBot({
        botName: botName.trim(),
        botToken: botToken.trim(),
        agentSlug,
        agentScope,
        sessionStrategy: strategy,
        idleTimeoutSecs: strategy === 'per_sender_sticky' ? idleMinutes * 60 : undefined,
        allowedUserIds: allowed,
      });
      open = false;
      onAdded?.();
    } catch (e) {
      error = errMsg(e);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Content class="max-w-md">
    <Dialog.Header>
      <Dialog.Title>Add Telegram bot</Dialog.Title>
      <Dialog.Description>
        Connect a bot token to an agent. The agent answers messages sent to the bot.
      </Dialog.Description>
    </Dialog.Header>

    <div class="space-y-4 py-2">
      <div class="space-y-1.5">
        <Label for="bot-name">Name</Label>
        <Input id="bot-name" bind:value={botName} placeholder="Support bot" autocomplete="off" />
        <p class="text-xs text-muted-foreground">A label to recognise this bot.</p>
      </div>

      <div class="space-y-1.5">
        <Label for="bot-token">Bot token</Label>
        <Input
          id="bot-token"
          type="password"
          bind:value={botToken}
          placeholder="123456:ABC-DEF…"
          autocomplete="off"
        />
        <p class="text-xs text-muted-foreground">From @BotFather. Stored as a secret.</p>
      </div>

      <div class="space-y-1.5">
        <Label>Agent</Label>
        <Select.Root type="single" bind:value={agentValue}>
          <Select.Trigger class="w-full">{selectedAgentLabel}</Select.Trigger>
          <Select.Content>
            {#each agentList as a (encode(a))}
              <Select.Item value={encode(a)}>{a.name} ({a.harness})</Select.Item>
            {/each}
          </Select.Content>
        </Select.Root>
      </div>

      <div class="space-y-1.5">
        <Label>Session strategy</Label>
        <Select.Root type="single" bind:value={strategy}>
          <Select.Trigger class="w-full">{strategyLabel}</Select.Trigger>
          <Select.Content>
            {#each STRATEGIES as s (s.value)}
              <Select.Item value={s.value}>{s.label}</Select.Item>
            {/each}
          </Select.Content>
        </Select.Root>
      </div>

      {#if strategy === 'per_sender_sticky'}
        <div class="space-y-1.5">
          <Label for="idle">Idle timeout (minutes)</Label>
          <Input id="idle" type="number" min="1" bind:value={idleMinutes} />
          <p class="text-xs text-muted-foreground">
            A sender's session closes after this much inactivity.
          </p>
        </div>
      {/if}

      <div class="space-y-1.5">
        <Label for="allowlist">Allowlist (optional)</Label>
        <Input
          id="allowlist"
          bind:value={allowlistRaw}
          placeholder="e.g. 12345678 98765432"
          autocomplete="off"
        />
        <p class="text-xs text-muted-foreground">
          Telegram user IDs allowed to use the bot. Empty = public.
        </p>
      </div>

      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
    </div>

    <Dialog.Footer>
      <Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
      <Button onclick={submit} disabled={submitting}>
        {submitting ? 'Adding…' : 'Add bot'}
      </Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
```

- [ ] **Step 2: Typecheck**

Run: `cd workspace && npm run check`
Expected: no errors. (`agents.list()` already exists in management-client; `WireAgent` now has `slug`.)

- [ ] **Step 3: Commit**

```bash
git add workspace/src/lib/AddBotDialog.svelte
git commit -m "feat(web): AddBotDialog — token + agent + strategy form"
```

---

## Task 6: ChannelsView page (frontend)

**Files:**
- Create: `workspace/src/lib/ChannelsView.svelte`

Loads bindings + connections, joins by `connection_id` to show the bot name,
and renders each with an enable toggle and delete.

- [ ] **Step 1: Create the view**

Create `workspace/src/lib/ChannelsView.svelte`:

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { MessageCircle, RefreshCw, Trash2, Plus, PanelLeft } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { channelsStore } from './channels.svelte';
  import { connectionsStore } from './connections.svelte';
  import AddBotDialog from './AddBotDialog.svelte';
  import { app } from './state.svelte';
  import { errMsg } from './utils';
  import type { ChannelBinding } from './channels.svelte';

  let { onOpenSidebar }: { onOpenSidebar?: () => void } = $props();

  let dialogOpen = $state(false);

  onMount(() => {
    void channelsStore.load();
    void connectionsStore.load();
  });

  // connection_id → bot name, for display.
  const nameById = $derived.by(() => {
    const m = new Map<string, string>();
    for (const c of connectionsStore.list) {
      if (c.kind === 'telegram_bot') m.set(c.id, c.name);
    }
    return m;
  });

  const strategyLabel: Record<string, string> = {
    per_sender_sticky: 'Per sender',
    persistent_one: 'One session',
    ephemeral: 'Ephemeral',
  };

  async function toggle(b: ChannelBinding) {
    try {
      await channelsStore.setEnabled(b.id, !b.enabled);
    } catch (e) {
      app.lastError = errMsg(e);
    }
  }

  async function remove(b: ChannelBinding) {
    try {
      await channelsStore.removeBot(b);
    } catch (e) {
      app.lastError = errMsg(e);
    }
  }
</script>

<div class="flex h-full min-h-0 w-full flex-col">
  <header class="flex items-center gap-2 border-b border-border/40 px-4 py-3 md:px-8">
    <Button
      variant="ghost"
      size="icon"
      class="md:hidden"
      onclick={() => onOpenSidebar?.()}
      aria-label="Show sidebar"
    >
      <PanelLeft class="size-4" />
    </Button>
    <h1 class="flex items-center gap-2 text-sm font-semibold">
      <MessageCircle class="size-4 text-muted-foreground" /> Channels
    </h1>
    <div class="ml-auto flex items-center gap-1">
      <Button
        variant="ghost"
        size="icon"
        onclick={() => void channelsStore.load(true)}
        aria-label="Refresh"
      >
        <RefreshCw class={['size-3.5', channelsStore.loading ? 'animate-spin' : '']} />
      </Button>
      <Button onclick={() => (dialogOpen = true)}>
        <Plus class="size-4" /> Add bot
      </Button>
    </div>
  </header>

  <div class="flex-1 overflow-y-auto px-4 py-6 md:px-8">
    <div class="mx-auto max-w-2xl space-y-3">
      {#if channelsStore.error}
        <p class="text-sm text-destructive">{channelsStore.error}</p>
      {:else if channelsStore.list.length === 0 && channelsStore.loaded}
        <p class="text-sm text-muted-foreground">
          No Telegram bots yet. Click “Add bot” to connect one.
        </p>
      {:else}
        {#each channelsStore.list as b (b.id)}
          <div class="flex items-center gap-3 rounded-md border border-border/40 px-4 py-3">
            <MessageCircle class="size-4 shrink-0 text-muted-foreground" />
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-medium">
                {nameById.get(b.connection_id) ?? 'Telegram bot'}
              </p>
              <p class="text-[11px] text-muted-foreground">
                {b.agent_slug} · {strategyLabel[b.session_strategy] ?? b.session_strategy}
                {#if b.allowed_user_ids.length > 0}· {b.allowed_user_ids.length} allowed{/if}
              </p>
            </div>
            <button
              type="button"
              onclick={() => void toggle(b)}
              aria-pressed={b.enabled}
              title={b.enabled ? 'Enabled — click to disable' : 'Disabled — click to enable'}
              class={[
                'rounded-full px-2.5 py-1 text-[11px] font-medium transition-colors',
                b.enabled
                  ? 'bg-primary/15 text-primary hover:bg-primary/25'
                  : 'bg-muted text-muted-foreground hover:bg-muted/70',
              ]}
            >
              {b.enabled ? 'Enabled' : 'Disabled'}
            </button>
            <Button
              variant="ghost"
              size="icon"
              onclick={() => void remove(b)}
              aria-label="Delete bot"
              class="text-destructive hover:bg-destructive/10"
            >
              <Trash2 class="size-4" />
            </Button>
          </div>
        {/each}
      {/if}
    </div>
  </div>
</div>

<AddBotDialog bind:open={dialogOpen} />
```

- [ ] **Step 2: Typecheck**

Run: `cd workspace && npm run check`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add workspace/src/lib/ChannelsView.svelte
git commit -m "feat(web): ChannelsView — list bots with enable toggle and delete"
```

---

## Task 7: Wire the /channels route and sidebar nav (frontend)

**Files:**
- Modify: `workspace/src/lib/App.svelte`
- Modify: `workspace/src/lib/SessionList.svelte`

- [ ] **Step 1: Add the route to `App.svelte`**

In `workspace/src/lib/App.svelte`:

(a) Import the view — add after the `ConnectionsView` import (line 11):

```ts
  import ChannelsView from './lib/ChannelsView.svelte';
```

(b) Extend the `Route` union (after the `connections` line, ~line 46):

```ts
    | { kind: 'channels' }
```

(c) In `parseRoute` (after the `/connections` line, ~line 57):

```ts
    if (window.location.pathname === '/channels') return { kind: 'channels' };
```

(d) In `pathFor` (after the `/connections` line, ~line 71):

```ts
    if (r.kind === 'channels') return '/channels';
```

(e) Add `'channels'` to `navKinds` (line 81):

```ts
  const navKinds = ['agents', 'scheduled', 'skills', 'connections', 'channels'] as const;
```

(f) Add a nav helper next to `openConnections` (~line 135):

```ts
  const openChannels = () => navigate({ kind: 'channels' });
```

(g) Pass it to `SessionList` (in the `<SessionList ... />` props block, after `onOpenConnections={openConnections}`):

```svelte
      onOpenChannels={openChannels}
```

(h) Render it — in the main `{#if route.kind === ...}` chain, after the `connections` branch (~line 372):

```svelte
      {:else if route.kind === 'channels'}
        <ChannelsView onOpenSidebar={() => (sidebarOpen = true)} />
```

- [ ] **Step 2: Add the nav entry to `SessionList.svelte`**

In `workspace/src/lib/SessionList.svelte`:

(a) Import the icon — add `MessageCircle` to the `@lucide/svelte` import (line 5-18):

```ts
    MessageCircle,
```

(b) Add the prop — in the `$props()` destructure (lines 47-71) add `onOpenChannels` to both the destructure and its type, and widen `activeNav`:

```ts
    onOpenConnections,
    onOpenChannels,
    activeNav = null,
```

and in the type block:

```ts
    onOpenConnections?: () => void;
    onOpenChannels?: () => void;
    activeNav?: 'agents' | 'scheduled' | 'skills' | 'connections' | 'channels' | null;
```

(c) Also widen the `navPill` snippet's `key` param type (line 557) and the collapsed-rail section. Change the snippet signature:

```svelte
      {#snippet navPill(
        key: 'agents' | 'scheduled' | 'skills' | 'connections' | 'channels',
        label: string,
        title: string,
        Icon: typeof Bot,
        onPick: () => void,
      )}
```

(d) Add the expanded nav pill — after the `connections` pill render (~line 605):

```svelte
          {#if onOpenChannels}
            {@render navPill('channels', 'Channels', 'Channels — Telegram bots', MessageCircle, onOpenChannels)}
          {/if}
```

(e) Add the collapsed-rail icon button — after the `onOpenConnections` rail button (~line 260):

```svelte
      {#if onOpenChannels}
        <Button
          variant="ghost"
          size="icon"
          onclick={onOpenChannels}
          aria-label="Channels"
          title="Channels — Telegram bots"
          class="text-muted-foreground hover:bg-sidebar-accent/60"
        >
          <MessageCircle />
        </Button>
      {/if}
```

- [ ] **Step 3: Typecheck + build**

Run: `cd workspace && npm run check && npm run build`
Expected: no errors, build succeeds.

- [ ] **Step 4: Commit**

```bash
git add workspace/src/lib/App.svelte workspace/src/lib/SessionList.svelte
git commit -m "feat(web): wire /channels route and sidebar nav entry"
```

---

## Task 8: Full verification (backend + frontend + manual)

**Files:** none (verification only)

- [ ] **Step 1: Backend gate**

Run:
```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test -p roy-management --no-fail-fast
```
Expected: all pass.

- [ ] **Step 2: Frontend gate**

Run:
```bash
cd workspace && npm run check && npm run build
```
Expected: no type errors, clean build.

- [ ] **Step 3: Manual verification against the running Docker stack**

The stack is already up (containers `roy-management` :8079, `roy-workspace` :8080, `roy-daemon`, `roy-gateway`). Rebuild the two affected images so the new management binary + frontend are live (use the project's existing build flow — e.g. `docker compose build roy-management roy-workspace && docker compose up -d roy-management roy-workspace`, or the repo's documented image build per INSTALL.md).

Then in a browser at `http://127.0.0.1:8080`:
1. Log in.
2. Open the new **Channels** entry in the sidebar.
3. Click **Add bot**, enter a name + a real bot token, pick an agent, keep `Per sender (sticky)` with 60 min, leave allowlist empty, submit.
4. Confirm the bot appears in the list with the agent slug + strategy.
5. Click the **Enabled** pill → it flips to **Disabled** (verify with
   `curl -s http://127.0.0.1:8079/internal/telegram-sources -H "Authorization: Bearer $ROY_INTERNAL_TOKEN"` — the bot drops out when disabled, reappears when re-enabled).
6. Delete the bot → it disappears and its connection is gone (`GET /connections` no longer lists it).

- [ ] **Step 4: Note the runtime prerequisite (not a code change)**

Document for the user: the bot rows exist in `agents.db`, but the bot only **replies** when `roy-inbound` runs with `ROY_INTERNAL_TOKEN` matching roy-management. `roy-inbound` is not in the current Docker stack — wiring it in is a separate deployment task (see spec “Out of scope”).

- [ ] **Step 5: Final commit (if any verification fixups were needed)**

```bash
git add -A
git commit -m "chore(web): channels UI verification fixups"
```

---

## Self-Review Notes

- **Spec coverage:** placement (`/channels` page — Task 7), create+list+delete+toggle (Tasks 5/6 + backend Task 2), frontend-orchestrated create with rollback (Task 4 `addBot`), delete also removes connection (Task 4 `removeBot`), optional allowlist (Task 5 dialog), PATCH toggle backend (Task 2). The agent-slug exposure (Task 1) is an added necessary correctness task discovered during planning — the spec assumed binding-by-agent "just works"; it needs the slug.
- **Out of scope honored:** no edit-existing-bot; `roy-inbound` deployment flagged as a separate task (Task 8 Step 4), not implemented.
- **Type consistency:** `ChannelBinding`/`NewChannelBinding`/`SessionStrategy` defined in Task 3 are the exact shapes consumed in Tasks 4–6; `WireAgent.slug` (Task 3) feeds `agentSlug` (Tasks 5→4→backend Task 2 validation). `channelsStore.setEnabled/removeBot/addBot` signatures match their call sites in ChannelsView.
