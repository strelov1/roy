# Telegram support: bind a bot to an agent, one sticky session per end-user

**Date:** 2026-05-29
**Status:** Approved (design), pending implementation plan
**Origin:** product request — "связать бота с агентом, чтобы каждый пользователь
получал свою сессию с этим агентом" (Telegram customer support). Realizes the
`Telegram-customer-support` roadmap channel listed for `roy-inbound` in `CLAUDE.md`.

## Problem

Today there are two ways an external Telegram user can reach roy, neither of which is
"a public bot, bound to a chosen agent persona, where every user gets their own
ongoing conversation":

- **`roy-gateway`** bridges Telegram to the daemon, but the harness is a single global
  config value, `system_prompt` is hard-coded `None` (no persona), and the
  `SessionBinder` is a flat `chat_id → session_id` JSON file. It is an interactive
  personal bridge, not a multi-tenant support bot bound to an agent.
- **`roy-inbound`** already has the right substrate — per-source session strategies
  (`ephemeral` / `persistent_one` / **`per_sender_sticky`**), a SQLite `bindings` table
  keyed `(source_id, sender_id) → session_id`, and `Fire`-over-socket dispatch — but it
  only ships an HTTP **webhook** channel, its `agent_id` is nominal (the resolver
  hard-codes `system_prompt: None` and uses one global harness), and it is configured
  by static TOML, not from the web UI where agents and connections actually live.

MCP is the wrong layer for this: `roy mcp serve` exposes daemon control as tools and
`roy mcp serve-connections` injects upstream MCP servers as the agent's **outbound**
tools. Neither models an **inbound human channel** whose messages flow into a session
and whose replies flow back to that human. That needs a channel adapter, not MCP.

## Goal

Add a **Telegram channel to `roy-inbound`** that:

1. Binds a Telegram bot to an **agent persona** (harness + system prompt), configured
   from the **web UI** in `roy-management`.
2. Gives **each Telegram sender their own session** with that agent
   (`per_sender_sticky`): first message spawns, subsequent messages resume, isolated
   per `(bot, chat_id)`.
3. Resolves the persona **live from `roy-management`** without `roy-inbound` reading
   `roy-management`'s database directly — the cross-crate link is a thin, read-only,
   config-only HTTP contract (boundary decision **A1**, see below).

**Non-goals (this spec):** retiring `roy-gateway`'s bespoke Telegram path; streaming
edit UX (deferred to slice 4); IMAP/WhatsApp channels; one-event-to-many-agents fan-out.

## Boundary decision (A1) and why it does not break isolation

The workspace boundary rule (`CLAUDE.md`): spokes depend on `roy-protocol` for wire
types and reach the **daemon only via its Unix socket**. A1 keeps both invariants:

- `roy-inbound`'s daemon access stays socket-only (`Fire`). The new HTTP call targets
  `roy-management` — a sibling service with its own legitimate HTTP API — **not** the
  daemon and **not** `roy` core internals.
- A1 **avoids** a cross-crate DB-ownership leak (the audit's real concern): `roy-inbound`
  never opens `agents.db`; `roy-management` stays the sole owner and resolves token +
  persona behind its own API.

The one genuinely new thing is the **first adapter→adapter runtime edge** (today the
topology is a clean star around the daemon). It is accepted deliberately and kept clean:
**control-plane only** (config, not session ops), **read-only**, DTOs single-sourced in
`roy-protocol`, consulted only at (re)load — never on the per-message hot path — and the
last-good config is cached in memory so `roy-management` downtime does not interrupt live
chats.

## Design

### Two-plane state model (distinct owners, no overlap)

| Plane | Question it answers | Store | Owner |
|---|---|---|---|
| **Config** | "which agent does bot B run, with what strategy?" | `channel_bindings` table in `agents.db` | `roy-management` (web-UI CRUD) |
| **Runtime** | "which session is sender S of bot B talking to?" | `bindings` table in `roy-inbound/state.db` | `roy-inbound` (already exists) |

The runtime plane is unchanged — `SessionResolver` + `BindingStore` already implement
`per_sender_sticky` keyed `(source_id, sender_id)`, surviving daemon restarts.

### 1. `roy-management` — control plane

**Telegram bot as a `connections` row.** A bot is created in the web UI as a connection
of a new kind `telegram_bot` (via the existing `Store::create_custom` path), with the
token in `secrets_json` (`{"bot_token": "..."}`). No new table for the bot itself; this
matches the existing "bots are sourced from `connections`" intent.

**New table `channel_bindings`** (new migration on `agents.db`):

```sql
CREATE TABLE channel_bindings (
  id                TEXT PRIMARY KEY,
  owner_id          TEXT NOT NULL REFERENCES users(id),
  channel_kind      TEXT NOT NULL,                 -- 'telegram' (room for more)
  connection_id     TEXT NOT NULL REFERENCES connections(id),  -- the bot
  agent_slug        TEXT NOT NULL,                 -- which persona to run
  agent_scope       TEXT NOT NULL,                 -- 'user' | 'team:<team_id>'
  session_strategy  TEXT NOT NULL,                 -- 'per_sender_sticky' (default) | 'persistent_one' | 'ephemeral'
  idle_timeout_secs INTEGER,                       -- required for per_sender_sticky
  allowed_user_ids  TEXT,                          -- JSON array; NULL/empty = public
  enabled           INTEGER NOT NULL DEFAULT 1,
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL,
  UNIQUE(connection_id)                            -- one bot ⇒ one binding
);
```

**Web-UI CRUD** (JWT-cookie auth, ACL by `owner_id`), alongside `connections.rs` routes:

- `GET /channel-bindings` — list the caller's bindings.
- `POST /channel-bindings` — create `{connection_id, agent_slug, agent_scope, session_strategy, idle_timeout_secs?, allowed_user_ids?}`. Validate: connection exists, is owned, kind `telegram_bot`; `agent_slug` resolves in `agent_scope`; sticky requires `idle_timeout_secs`.
- `PUT /channel-bindings/{id}` / `DELETE /channel-bindings/{id}`.

**Internal endpoint** (loopback, returns secrets — gated):

```
GET /internal/telegram-sources        Authorization: Bearer $ROY_INTERNAL_TOKEN
→ 200 [ TelegramSource, ... ]          (only enabled telegram bindings)
```

The handler joins, per enabled binding: `connections.get(connection_id)` →
`secrets.bot_token`; resolves the agent file via `agents.rs` from `(owner_id,
agent_scope, agent_slug)` → `{harness, system_prompt(=body), model}`. Bindings whose
connection or agent fails to resolve are **skipped with a warn**, not fatal.

`ROY_INTERNAL_TOKEN` (≥32 bytes) is read at startup. If unset, the internal endpoint is
not mounted (Telegram support simply stays off); the rest of `roy-management` is
unaffected. Bind the endpoint to loopback.

### 2. `roy-protocol` — shared contract

A new module (e.g. `roy-protocol::channel`) holds the DTOs both sides serialize, so the
contract has one definition and adds no new ad-hoc codec:

```rust
pub struct TelegramSource {
    pub source_id: String,            // "tg:<connection_id>"
    pub bot_token: String,
    pub harness: String,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub session_strategy: SessionStrategyWire,   // ephemeral | persistent_one | per_sender_sticky { idle_timeout_secs }
    pub allowed_user_ids: Vec<i64>,   // empty = public
}
```

`SessionStrategyWire` mirrors `roy-inbound`'s `SessionStrategyConfig` (kept in protocol
so both crates share it). Framing reuses `roy-protocol::wire` conventions; transport is
plain HTTP+JSON (not the daemon's newline framing).

### 3. `roy-inbound` — the Telegram channel

New module `crates/roy-inbound/src/channels/telegram/`:

**`ManagementSourceProvider`** — fetches `GET /internal/telegram-sources` on startup and
polls every ~30s (matching `roy-management`'s 30s agents cache). It owns:

- an `Arc<RwLock<HashMap<source_id, ResolvedSource>>>` consumed by the router/resolver
  and the reply hook (carries harness, system_prompt, strategy, allowed_user_ids);
- a `HashMap<source_id, Bot + JoinHandle>` of live teloxide tasks.

On each refresh it **reconciles**: start a teloxide task for added sources, abort it for
removed ones, update the in-memory persona/strategy for changed ones. The token lives
**only in memory** (never written to `roy-inbound`'s disk — preserves "secret in one
place"). `roy-management` unreachable at startup → retry with backoff; unreachable
mid-run → keep running with the current map.

**`TelegramPublisher`** (`Publisher`) — one teloxide dispatcher per bot (loop ported
from `roy-gateway/telegram.rs`). Maps an inbound message to
`InboundEvent { source_id = "tg:<conn_id>", sender_id = chat_id.to_string(),
payload = {"text":…, "user_id":…}, reply: ReplyHandle::Noop }`. Access control: if
`allowed_user_ids` is non-empty and the sender's `user_id` is absent, the message is
ignored; empty list = public. The `Message → InboundEvent` mapping is a pure function
(unit-testable without a live bot).

**`TelegramReplyHook`** (`ReplyHook`, registered in `ReplyHookRegistry` under kind
`"telegram"`) — replies out-of-band (the publisher used `ReplyHandle::Noop`). Its factory
closes over the provider's bot map; `make(kind, EventRef)` resolves `(bot, chat_id)` from
`source_id`/`sender_id`. Slice 1 implements `on_finish` only: `FireOutcome::Ok` →
send `assistant_text` to the chat; errors → a friendly `⚠ …` message. Slice 4 adds
`on_turn_event` streaming.

**Persona flows into Spawn (the core fix).** Today `SessionResolver::new(bindings,
harness)` uses one global harness and emits `FireTarget::Spawn { system_prompt: None }`.
Change: a source carries `harness` + `system_prompt`, threaded through `FireSpec` into the
resolver so it emits `FireTarget::Spawn { harness, system_prompt: Some(...) }`. Make it
additive — when a `FireSpec` carries no persona (the webhook path), fall back to today's
global harness + `None`, so the existing webhook channel is unchanged. The runtime
`bindings` upsert (storing `agent_id`, `strategy`, `session_id`) is unchanged.

**Routing.** The dispatcher's router consults the dynamic `ResolvedSource` registry for
`tg:*` sources (returning a `FireSpec` carrying the persona) and falls back to the
existing TOML `ConfigRouter` for webhook sources. `source_id` namespaces (`tg:` prefix)
guarantee no collision between the two source kinds.

**Process config.** Add a `[management]` section to inbound config (`url`, default
`http://127.0.0.1:<mgmt_port>`) and read `ROY_INTERNAL_TOKEN` from env. The Telegram
channel coexists with the webhook channel in the same `roy inbound` process; the daemon
socket it already holds carries the `Fire` calls.

### End-to-end data flow

```
Telegram user S → bot B
  └ TelegramPublisher (teloxide)  →  InboundEvent{ source_id="tg:<B>", sender_id=chat_id, text }
       └ Dispatcher → Router resolves source via registry → FireSpec{ harness, system_prompt, strategy }
            └ SessionResolver: (source_id, sender_id) in bindings?
                 hit & fresh → Fire{ Resume session }      miss/expired → Fire{ Spawn harness+system_prompt }
                 └ daemon (Unix socket) → FireDone{ assistant_text }
                      on Spawn-Ok: upsert binding (sender_id → new session_id)
                      └ TelegramReplyHook.on_finish → send assistant_text to chat_id
```

### Defaults (locked unless changed in review)

- **Access:** support bot is **public** by default (`allowed_user_ids` empty); allowlist optional.
- **`sender_id` = Telegram `chat_id`** (one DM user = one session).
- **Reply mode:** **final buffered answer** (`on_finish`) first; streaming edits
  (`on_turn_event` + ported `DraftStream`/typing/`/cancel`) is slice 4.

## Build slices & verification

Each slice builds and tests green (`cargo fmt --all -- --check && cargo build
--workspace --all-targets && cargo test --workspace --no-fail-fast`).

1. **Control plane (`roy-management` + `roy-protocol`).** `telegram_bot` connection kind;
   `channel_bindings` migration + CRUD; `/internal/telegram-sources` resolver + token
   gate; `TelegramSource`/`SessionStrategyWire` DTOs in `roy-protocol`.
   *Verify:* HTTP tests — create a `telegram_bot` connection + a binding, assert the
   internal endpoint returns the resolved token + persona; auth gate rejects without token.
2. **Channel skeleton (`roy-inbound`).** `ManagementSourceProvider` (fetch once, no poll);
   `TelegramPublisher` (single bot); `TelegramReplyHook` (`on_finish`); persona threaded
   into `FireTarget::Spawn`; `per_sender_sticky` with the real agent.
   *Verify:* unit-test `Message → InboundEvent`; test `TelegramReplyHook` via a mock
   `Replier` (lift the trait from `roy-gateway`); provider tested against a stub endpoint;
   one manual E2E against a real test bot.
3. **Reconciliation & resilience.** 30s poll; start/stop/refresh teloxide tasks on
   binding changes; retry/backoff when `roy-management` is down; live chats survive
   mid-run outages.
   *Verify:* provider reconcile unit tests (added/removed/changed source diffs); outage
   simulation keeps the in-memory map.
4. **(Phase 2) Streaming UX.** Port `DraftStream`, `TypingKeepalive`, `/cancel`,
   long-message splitting from `roy-gateway` into `TelegramReplyHook.on_turn_event`.

## Testing approach

- `roy-management`: HTTP handler tests with the existing `MockDaemonClient`; binding
  validation (bad scope, missing connection, sticky without timeout); internal-endpoint
  auth + resolution.
- `roy-inbound`: pure `Message → InboundEvent` mapping; reply hook via mock `Replier`;
  source-provider reconcile diffs against a stubbed endpoint; resolver persona-into-Spawn
  regression. Real teloxide network paths stay out of unit tests (manual smoke).

## Scope boundaries (explicitly NOT in this work)

- Retiring `roy-gateway`'s bespoke Telegram path (separate follow-up once this stabilizes).
- IMAP / WhatsApp channels; one-event → many-agents fan-out; persisting inbound events
  for replay.
- The bot↔one-specific-web-session bridge (the original framing, scenario A) — separate
  feature if still wanted.
- Encryption-at-rest / secret rotation for `connections.secrets` (unchanged from today).

## Open choices

- **Internal-endpoint auth transport:** loopback TCP + `ROY_INTERNAL_TOKEN` bearer
  (proposed) vs a Unix-socket listener on `roy-management` (no token on the wire).
  Chosen for now: **loopback + bearer** — simplest, and the token already lives in a
  `0600` DB on the same host/operator.
- **Poll vs push for source refresh:** 30s poll (proposed, mirrors the agents cache) vs
  an ETag/long-poll or a management→inbound notification. Chosen: **30s poll**.
- **Per-user manual reset:** offer a `/new` command to let a sender start a fresh thread
  (delete their sticky binding) — deferred; add if requested.
