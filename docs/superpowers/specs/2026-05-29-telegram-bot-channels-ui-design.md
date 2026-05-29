# Telegram Bot Channels UI — Design

**Date:** 2026-05-29
**Status:** Approved (design), pending implementation plan

## Problem

Connecting a Telegram support bot today is API-only: the user must `POST
/connections` (kind `telegram_bot`) and then `POST /channel-bindings` by
hand (curl). The web frontend (`workspace/`, SvelteKit) has no UI for it —
its `/connections` page covers MCP stdio connections only. We want a
first-class UI to add, list, delete, and enable/disable Telegram support
bots.

## Decisions

- **Placement:** a new dedicated page `/channels` (sidebar nav item), not an
  extension of `/connections`. A Telegram support bot is a distinct mental
  model ("a bot answered by an agent"), and `/connections` is MCP-provider
  shaped (catalog from `connections.yaml`), which Telegram has no entry in.
- **Scope (MVP):** create + list + delete + enable/disable toggle. No edit of
  an existing bot.
- **Create flow:** the frontend orchestrates the two existing API calls
  (`POST /connections` → `POST /channel-bindings`) and rolls back the
  connection via `DELETE` if the binding call fails. No new atomic
  "create-channel" backend endpoint — that would be extra code for a rare
  race; frontend orchestration with rollback is simpler and reuses ready
  endpoints.
- **Delete:** deleting a bot deletes the binding **and** its connection, so we
  don't leave orphaned bot tokens in the DB.
- **Allowlist:** optional field in the add-bot form. Empty = public bot;
  non-empty = restrict to those Telegram user IDs. Useful for limiting a bot
  to staff during rollout; cheap because the backend already accepts
  `allowed_user_ids`.

## Backend changes (roy-management)

The only backend gap is the enable/disable toggle. Routes today
(`channel_bindings.rs:356`) are `GET/POST /channel-bindings` and
`GET/DELETE /channel-bindings/{id}`; `enabled` is always inserted `true` and
only read (`list_enabled_telegram`). Add:

- `Store::set_enabled(owner_id, id, enabled) -> Result<ChannelBinding, StoreError>`
  — `UPDATE channel_bindings SET enabled = ?, updated_at = ? WHERE owner_id = ? AND id = ?`,
  returns the updated row (or `NotFound`).
- Handler `PATCH /channel-bindings/{id}` with body `{ "enabled": bool }`,
  mounted on the existing `/{id}` route via `.patch(update_handler)`.
- Unit test alongside the existing `#[cfg(test)]` block: create → toggle off →
  assert `list_enabled_telegram` drops it → toggle on → reappears.

`POST /connections` already accepts `kind: "telegram_bot"` (token in
`secrets.bot_token`, `config: {}` — `validate_config` only requires an
object). No change needed there.

## Frontend changes (workspace/src)

1. **`lib/management-client.ts`**
   - Widen `Connection.kind` from the hardcoded `'mcp_stdio'` to a
     discriminated union `'mcp_stdio' | 'telegram_bot'`, with `config` typed
     per kind (telegram config is `{}`). Existing MCP code keeps reading
     `mcp_stdio`.
   - Allow a `telegram_bot` shape in the custom connection create body.
   - Add `ChannelBinding` and `NewChannelBinding` types (mirror the Rust
     structs: `connection_id`, `agent_slug`, `agent_scope`,
     `session_strategy`, `idle_timeout_secs?`, `allowed_user_ids: number[]`,
     `enabled`, timestamps).
   - Add a `channelBindings` API namespace: `list()`, `create(body)`,
     `remove(id)`, `setEnabled(id, enabled)` (the new PATCH).

2. **`lib/channels.svelte.ts`** — new `LoadableStore<ChannelBinding>` subclass
   (mirrors `connections.svelte.ts`): `load`, `create`, `remove`,
   `setEnabled` with optimistic local update.

3. **`lib/ChannelsView.svelte`** — the page. Loads channel bindings +
   connections (filtered to `kind === 'telegram_bot'`), joins by
   `connection_id` to show bot name. Renders a list (bot name, bound agent,
   session strategy, enabled toggle, delete) and an "Add bot" button.

4. **`lib/AddBotDialog.svelte`** — form: bot token → agent picker (from
   `agents.list()`) → session strategy (default `per_sender_sticky` with
   `idle_timeout_secs`) → optional allowlist (comma-separated Telegram user
   IDs). On submit: orchestrate create-connection → create-binding with
   rollback.

5. **`App.svelte` + `lib/SessionList.svelte`** — wire a `/channels` route
   following the existing pattern: extend the `Route` union, `parseRoute`,
   `pathFor`, `navKinds`, add `openChannels`/`onOpenChannels`, render
   `ChannelsView`, add a sidebar nav entry.

## Data flow

- `ChannelsView` mount → `channelBindings.list()` + `connections.list()`;
  join by `connection_id`.
- Add bot → `connections.create({ name, kind: 'telegram_bot', config: {},
  secrets: { bot_token } })` → `channelBindings.create({ connection_id,
  agent_slug, agent_scope, session_strategy, idle_timeout_secs,
  allowed_user_ids })`; on binding failure, `connections.remove(connection_id)`.
- Toggle → `channelBindings.setEnabled(id, next)` (optimistic).
- Delete → `channelBindings.remove(id)` then `connections.remove(connection_id)`.

## Error handling

- Duplicate binding (`UNIQUE(connection_id)`) or invalid token surfaces as an
  `HttpError` → shown via the existing `app.lastError` toast.
- 401 is already handled globally in `management-client.ts` `request()`.
- Partial create (connection made, binding failed) is rolled back by deleting
  the connection; if rollback itself fails, surface the error so the user can
  clean up manually.

## Testing

- Backend: unit test for `set_enabled` + the `PATCH` handler.
- Frontend: no view-level unit tests exist in this project; verify manually
  against the running Docker stack (management :8079, workspace :8080) — add a
  bot, see it listed, toggle, delete.

## Out of scope (YAGNI)

- Editing an existing bot (only create/delete/toggle).
- **Running `roy-inbound` in Docker.** The UI writes connection + binding rows
  to `agents.db`, but the bots only actually answer when `roy-inbound` is
  running with `ROY_INTERNAL_TOKEN` matching roy-management. `roy-inbound` is
  not in the current Docker deployment. This is a **separate deployment task**
  and a prerequisite for "the bot actually replies" — tracked outside this UI
  spec.
