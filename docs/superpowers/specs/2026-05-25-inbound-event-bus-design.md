# Inbound Event Bus — design

Date: 2026-05-25
Status: design (pre-implementation)

## Context

Today `roy` has two unrelated inbound paths:

- `roy-gateway` — Telegram long-poll + WebSocket relay. Hard-codes a single
  channel (`teloxide`-driven Telegram bot) into the same module that resolves
  sessions and streams replies back.
- `roy-scheduler` — cron/oneshot timer that fires agents on a schedule.

Both end up driving `ClientCommand::Fire` against the daemon, but they share
no abstractions. Adding a third inbound source today (HTTP webhook from a
3rd-party SaaS, IMAP-driven email reply, WhatsApp Business inbound) would
mean either growing `roy-gateway` into a kitchen sink or copy-pasting the
spawn/resume/stream pipeline into a new crate.

The goal of this spec is to introduce a single inbound substrate so each new
channel only contributes its own listener and reply formatter — the rest
(routing to an agent, session lifecycle, fire dispatch, error reporting) is
shared infrastructure.

## Goals

1. New crate `roy-inbound` hosts the substrate plus the first concrete
   channel (HTTP webhook).
2. Channels are pure publishers — they normalize their native event into one
   `InboundEvent` and hand it to the bus. They do not know about agents,
   sessions, daemon, or replies-after-the-fact.
3. Reply paths are pluggable per channel-kind (`ReplyHook` trait). Sync HTTP
   response, fire-and-forget, streaming edits are all expressible without
   touching the dispatcher.
4. Session lifecycle (ephemeral / persistent-one / per-sender-sticky)
   handled centrally; one persisted binding table covers all current and
   future channels.
5. `roy-gateway` keeps working unchanged. No regression in the Telegram
   customer-support scenario: one bot, one persona agent, sticky session
   per chat.
6. The daemon, `roy-scheduler`, `roy-agents`, and `roy-management` are
   untouched. `roy-inbound` only talks to the daemon through the Unix
   socket, same boundary rule as the other adapter crates.

## Non-goals

- Migrating `roy-gateway` (Telegram, WS relay) onto the bus — separate spec
  once the bus matures on webhook.
- Cron/oneshot triggers — `roy-scheduler` stays as-is.
- Outbound tools / credentials vault — orthogonal, separate spec track.
- Multi-process bus (NATS, Redis pub/sub). Bus is in-process only.
- Persisting inbound events for replay across daemon restarts. Channels
  with at-least-once semantics upstream (HTTP retries, Telegram getUpdates
  offset, IMAP UIDNEXT) take care of it themselves.
- UI for inbound configuration — first iteration uses a TOML config file.
- Fan-out (one event → multiple agents). The router contract supports it
  in principle but the first router implementation is one-event one-fire.

## Terminology

- **Channel** — a kind of inbound transport. `webhook`, `telegram`, `imap`,
  `whatsapp`. Each channel ships a `Publisher` implementation and a
  `ReplyHook` factory.
- **Source** — a configured instance of a channel. One running webhook on
  path `/orders` is one source. One Telegram bot is one source. Sources
  are identified by a stable `source_id` string from the TOML config.
- **InboundEvent** — the in-process message that travels on the bus.
- **Bus** — `tokio::sync::mpsc` channel with a single consumer
  (`InboundDispatcher`). The dispatcher is the only place that talks to
  the daemon socket and to the binding store.
- **Router** — decides what to do with an `InboundEvent`. Default
  implementation: lookup `source_id` in TOML config, return the bound
  `agent_id` and the configured session strategy.
- **FireSpec** — the dispatcher-internal "what to send to the daemon"
  shape, produced by the router.
- **ReplyHook** — receives the `TurnEvent` stream produced by the fire and
  delivers something back through the `ReplyHandle` from the original
  `InboundEvent`.
- **ReplyHandle** — typed token attached to the event by the publisher.
  Encodes how to reply. `Noop` if the channel is one-way.
- **Binding** — a persisted `(source_id, sender_id) → session_id` row. Only
  written when the source's session strategy is `persistent_one` or
  `per_sender_sticky`.

## Architecture

```
crates/
  roy-inbound/                    ← NEW
    src/
      lib.rs                      # pub use re-exports, run()
      bus.rs                      # InboundEvent, BusSender, dispatcher loop
      router.rs                   # Router trait + ConfigRouter default impl
      session.rs                  # SessionResolver: strategy → FireTarget
      reply.rs                    # ReplyHandle, ReplyHook trait
      store/
        bindings.rs               # sqlx CRUD on bindings table
        db.rs                     # open + migrations
      channels/
        mod.rs                    # Publisher trait
        webhook/
          mod.rs                  # WebhookPublisher (axum router)
          reply.rs                # WebhookReplyHook (writes oneshot HTTP)
          config.rs               # WebhookConfig parser
      cli.rs                      # pub async fn run(args) for roy-cli
      config.rs                   # InboundConfig (TOML)
    migrations/sqlite/
      0001_initial.sql            # bindings table
```

External boundary: depends on `roy` only for `ClientCommand`, `ServerEvent`,
`FireTarget`, `TurnEvent`, `StopReason`, `PidLock`. No `SessionManager`,
`Engine`, `Journal`, `Transport` imports.

`roy-cli` gains a `roy inbound --config <path>` subcommand that calls
`roy_inbound::cli::run(args)`, by analogy with `roy gateway` and
`roy scheduler`.

## Components

### InboundEvent

```rust
pub struct InboundEvent {
    pub id: Uuid,
    pub source_id: String,
    pub source_kind: String,
    pub sender_id: String,
    pub payload: Value,
    pub received_at: DateTime<Utc>,
    pub reply: ReplyHandle,
}
```

`sender_id` is opaque to the bus and the router. For webhook it is the
client IP (or an explicit value the publisher extracts from the body, e.g.
account id from JWT). For Telegram it would be the chat id. For IMAP it
would be the From-address. The convention is "stable identifier of the
remote party as that channel knows it".

`payload` is a `serde_json::Value`. Each channel normalizes its native
representation (Telegram update, HTTP body, MIME message) into JSON that
its templating layer can render into the agent prompt. Concretely:

- webhook: `{"method": "...", "path": "...", "headers": {...}, "body": ...}`
- telegram (future): `{"text": "...", "from": {...}, "message_id": ...}`

The dispatcher does not inspect `payload`; only the router (which renders
the prompt) and the reply hook (which may quote the input) do.

### ReplyHandle

```rust
pub enum ReplyHandle {
    Noop,
    HttpSync {
        responder: oneshot::Sender<HttpReply>,
    },
    // Future: TelegramEdit { chat_id, message_id, bot: Arc<Bot> }
    //         WhatsApp { conversation_id, client: Arc<WaClient> }
    //         Smtp { in_reply_to: MessageId, client: Arc<SmtpClient> }
}
```

`Noop` is sent for one-way channels and when the publisher does not need a
reply (webhook configured as fire-and-forget). The reply hook is still
invoked for `Noop` events — it sees the result, logs/audits, returns.

### Publisher

```rust
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Run until cancelled. Publishes InboundEvents into `bus`.
    /// Implementations own their listener task; returning Err means the
    /// publisher failed permanently.
    async fn run(
        self: Arc<Self>,
        bus: BusSender,
        cancel: CancellationToken,
    ) -> Result<()>;
}
```

The publisher does not return events; it pushes them. It owns its own
listener (axum server, telegram long-poll loop, imap IDLE socket). The
supervisor spawns one tokio task per configured source.

If a publisher panics, the supervisor logs and restarts it with exponential
backoff (cap 30s). A publisher that returns `Err` from `run` is logged as
permanently failed and not restarted — operator intervention required.

### Bus

`tokio::sync::mpsc::Sender<InboundEvent>` with a capacity (config, default
256). Single consumer is `InboundDispatcher`. Publishers backpressure on
`send().await` — a slow dispatcher means slow ack to upstream callers.

This is deliberate: spike absorption is the upstream's job (HTTP load
balancer, telegram polling buffer). The bus is a queue, not an absorber.

### Router

```rust
#[async_trait]
pub trait Router: Send + Sync {
    /// Decide what to do with an event. Returning None drops the event
    /// after invoking the reply hook with a `RouteRejected` outcome so
    /// the publisher can reply 404/no-op.
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec>;
}

pub struct FireSpec {
    pub agent_id: String,
    pub prompt: String,
    pub session_strategy: SessionStrategy,
    pub tags: BTreeMap<String, String>,
}

pub enum SessionStrategy {
    Ephemeral,
    PersistentOne,
    PerSenderSticky { idle_timeout: Duration },
}
```

Default implementation `ConfigRouter` is constructed from the TOML config.
It looks up `event.source_id`, finds the bound `agent_id`, renders the
prompt from a simple template (initially: a fixed `template` string with
`{{payload.<path>}}` substitutions; the renderer is a tiny utility, not a
full templating engine), attaches tags
(`roy-inbound:source_id`, `roy-inbound:source_kind`, `roy-inbound:event_id`,
`roy-inbound:sender_id`).

`agent_id` is verified against `roy-agents` on dispatcher startup, not on
every event — if config references a deleted agent, the dispatcher refuses
to start. Runtime deletion is out of scope (operator restarts inbound).

### SessionResolver

Centralized translation of `SessionStrategy` into a `FireTarget`. Reads the
bindings table; writes are deferred to the dispatcher because the
`session_id` is only known after a successful Spawn returns.

```rust
pub struct SessionResolver {
    bindings: BindingStore,
}

pub struct PendingBinding {
    pub source_id: String,
    pub sender_id: String,
    pub agent_id: String,
    pub strategy: SessionStrategy,
}

impl SessionResolver {
    pub async fn resolve(&self, source_id: &str, sender_id: &str,
                        agent_id: &str, strategy: &SessionStrategy)
        -> Result<(FireTarget, Option<PendingBinding>)>;
}
```

`PendingBinding` is `Some` only when the resolver produces `FireTarget::Spawn`
under a sticky strategy. After the fire succeeds, the dispatcher calls
`bindings.upsert(pending, session_id)` with the actual `session_id` from the
`FireDone` event. If the fire fails, no binding is written.

Logic:

| Strategy | Lookup | If hit | If miss |
|---|---|---|---|
| Ephemeral | (no lookup) | n/a | `FireTarget::Spawn`; no `PendingBinding` |
| PersistentOne | `(source_id, sender_id="*")` | `FireTarget::Resume{session_id}` | `FireTarget::Spawn` + `PendingBinding` |
| PerSenderSticky | `(source_id, sender_id)` | check `last_active_at` vs `idle_timeout`; if expired — Spawn + `PendingBinding` (will overwrite); else Resume | `FireTarget::Spawn` + `PendingBinding` |

On `NoSession` from the daemon (session was evicted between fires), the
dispatcher clears the binding and retries once as `Spawn` — the same
pattern `roy-scheduler` already uses in `run_fire_for_agent` for persistent
agents. The retry logic lives in the dispatcher loop, not in the resolver.

`last_active_at` is updated after every successful fire, so
`PerSenderSticky` expiry is from last activity, not from binding creation.

### ReplyHook

```rust
#[async_trait]
pub trait ReplyHook: Send + Sync {
    async fn on_turn_event(&mut self, ev: &TurnEvent) -> Result<()>;

    /// Called exactly once, after the terminal Result or fire error.
    async fn on_finish(
        self: Box<Self>,
        outcome: FireOutcome,
        reply: ReplyHandle,
    ) -> Result<()>;
}

pub enum FireOutcome {
    Ok { assistant_text: String, cost_usd: Option<f64>, stop_reason: StopReason },
    DaemonError { code: ErrorCode, message: String },
    Timeout { partial_text: Option<String> },
    Cancelled,
    RouteRejected,
}
```

Per-channel reply hook implementations decide which `TurnEvent`s they care
about and how to deliver the final outcome. The webhook reply hook (first
implementation) ignores intermediate `TurnEvent`s and on `on_finish` sends
the assistant text (or error JSON) through `ReplyHandle::HttpSync`.

The dispatcher constructs the reply hook *per event* (it owns mutable state
per turn, e.g. accumulated tokens for streaming-edit channels in the
future). The construction is delegated to a per-channel factory registered
at supervisor startup.

### InboundDispatcher

Single tokio task. Owns:

- a `ConnFactory` that opens fresh Unix-socket connections to `roy serve`
  (one per fire; same pattern as `roy-gateway`'s `RealConnFactory`),
- the `BindingStore`,
- the `Router` and the map of per-channel `ReplyHookFactory`s,
- the `SessionResolver`.

Pseudocode:

```rust
while let Some(event) = bus.recv().await {
    let Some(fire) = router.route(&event).await else {
        let hook = factories.make(&event.source_kind);
        hook.on_finish(FireOutcome::RouteRejected, event.reply).await?;
        continue;
    };

    let (target, binding_write) = resolver
        .resolve(&event.source_id, &event.sender_id, &fire.session_strategy)
        .await?;

    let mut conn = conn_factory.open().await?;
    let outcome = run_fire(&mut conn, target, fire.prompt, fire.tags,
                           hook_factory.make(&event.source_kind), event.reply)
                  .await;

    if let Some(write) = binding_write {
        bindings.upsert(write, session_id_from(outcome)).await?;
    }
}
```

`run_fire` is the daemon-socket loop: `ClientCommand::Fire` → read
`ServerEvent`s → call `on_turn_event` per inner `TurnEvent` → call
`on_finish` on terminal `FireDone` / `FireError`.

If the dispatcher itself panics, the whole `roy-inbound` process exits
(supervised by systemd/launchd in production). This is deliberate — the
dispatcher holding partial state across panics would corrupt bindings.

## State

One SQLite database at `~/.local/state/roy-inbound/state.db`. Override via
`ROY_INBOUND_DB` env var. Migrations live in
`crates/roy-inbound/migrations/sqlite/`, applied at process start (same
pattern as scheduler).

Schema:

```sql
CREATE TABLE bindings (
    id              TEXT PRIMARY KEY,
    source_id       TEXT NOT NULL,
    sender_id       TEXT NOT NULL,
    session_id      TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    strategy        TEXT NOT NULL,           -- 'persistent_one' | 'per_sender_sticky'
    created_at      TEXT NOT NULL,
    last_active_at  TEXT NOT NULL,
    UNIQUE(source_id, sender_id)
);

CREATE INDEX bindings_by_last_active ON bindings(last_active_at);
```

No `events_log` table in this spec. Event durability is not a goal.
Per-fire history is already covered by the daemon journal and (for
scheduler) the `fires` table — both findable through their respective
session ids and tags.

## Config

TOML at `~/.config/roy/inbound.toml`, location overridable via CLI flag.

```toml
[bus]
capacity = 256

[[sources]]
id = "orders"
kind = "webhook"
agent_id = "order-processor"
session = "ephemeral"
fire_timeout_secs = 600
template = "New order: {{payload.body}}"

  [sources.webhook]
  path = "/webhooks/orders"
  secret_env = "ORDERS_WEBHOOK_SECRET"   # HMAC validation; absent means no auth
  reply_mode = "sync"                    # "sync" | "async"

[[sources]]
id = "classifier"
kind = "webhook"
agent_id = "classifier-bot"
session = "ephemeral"
fire_timeout_secs = 60
template = "Classify: {{payload.body.text}}"

  [sources.webhook]
  path = "/classify"
  reply_mode = "sync"

[server]
bind = "127.0.0.1:8090"
```

`fire_timeout_secs` is per-source so a fast classifier endpoint doesn't
inherit a slow batch source's timeout. Default if omitted: 600s.

Validation rules (enforced on config load — refusing to start on violation,
no graceful degradation):

- `id` is unique within the config
- `agent_id` exists in `roy-agents` store
- `session` is one of the known strategies
- For `per_sender_sticky` an `idle_timeout` field is required
- Channel-specific sub-table required for the declared `kind`
- Webhook paths are unique within the config

## Error handling

| Failure | Behavior |
|---|---|
| Publisher panic | task supervised, restart with exponential backoff (cap 30s); 3 consecutive failures within 60s → publisher marked permanently failed, logged at error level |
| Publisher returns Err from run | logged at error level, no restart |
| Bus full | `send().await` blocks the publisher. Webhook: client sees HTTP 503 after a configurable in-publisher timeout (default 5s). Other channels apply their own backpressure (telegram: getUpdates loop blocks). |
| Router returns None | reply hook invoked with `RouteRejected`. Webhook reply hook sends HTTP 404. |
| Daemon socket open fails | `on_finish(DaemonError{...})`, binding not written |
| Daemon returns error in stream | `on_finish(DaemonError{...})`, binding not written |
| Daemon returns NoSession on Resume | dispatcher clears binding row, retries once as Spawn, then proceeds as usual |
| Fire timeout (configurable per source, default 600s) | `on_finish(Timeout{partial_text})`, binding not written |
| ReplyHook returns Err | logged at error level, dispatcher continues with next event |
| ReplyHook panic | logged at error level, dispatcher continues |
| Dispatcher panic | process exits, supervised restart |

No event-level retries. If a publisher receives an event whose dispatch
failed, that is the upstream's problem (HTTP 5xx invites the caller to
retry; telegram's getUpdates offset stays unadvanced).

## Testing

| Layer | Coverage |
|---|---|
| Unit — `ConfigRouter` | template rendering; source_id → fire_spec; unknown source → None |
| Unit — `SessionResolver` | each strategy resolves to right `FireTarget`; binding upsert / expiry; NoSession retry logic |
| Unit — `BindingStore` | CRUD + uniqueness + expiry query |
| Unit — `WebhookReplyHook` | OK outcome → 200 + body; error outcome → 5xx + JSON; `RouteRejected` → 404 |
| Integration — `roy-inbound` against mock daemon | POST → ClientCommand::Fire sent with right tags → mock returns FireDone → HTTP 200 with assistant text. Reuses `spawn_mock_daemon` pattern from `roy-scheduler`. |
| Integration — sticky session | first POST spawns + writes binding; second POST resumes; binding `last_active_at` updated |
| Integration — `NoSession` fallback | mock daemon returns NoSession first then FireDone — dispatcher clears binding, retries, eventually succeeds |
| End-to-end | webhook POST → real daemon + fake ACP agent → assert HTTP response carries assistant text |
| Regression | `roy-gateway` tests untouched — must still pass to confirm Telegram path unaffected |

## Migration plan

This is additive. No existing data shapes change. New users:

1. Add `~/.config/roy/inbound.toml` with at least one source.
2. Ensure `roy serve` is running.
3. Start `roy inbound --config ~/.config/roy/inbound.toml` (or via
   systemd/launchd unit).
4. Tail `roy-inbound` logs; first POST creates the bindings table by
   running migrations on demand.

No data import. `roy-gateway` is unaffected; users on Telegram-only
deployments do not need to change anything.

## Open questions

These do not block the spec but will need decisions during the
implementation plan:

1. **Templating engine**. First iteration uses a hand-rolled
   `{{payload.json.path}}` substituter. If demand for conditionals/loops
   appears, swap to `minijinja` (already in the wider Rust ecosystem;
   small dep). Spec leaves the engine pluggable behind a private trait.
2. **Webhook auth**. First iteration: HMAC-SHA256 of the raw body, key
   from `secret_env`. Bearer tokens / static API keys can come later.
3. **Per-source concurrency limits**. Not in v1. The bus capacity is the
   only knob. Add `max_inflight` per source later if needed.
4. **Observability**. Spec does not mandate metrics. `tracing` spans
   covering publish → route → fire → reply are mandatory; Prometheus
   exporters are out of scope.
5. **Idle session sweep**. `PerSenderSticky` bindings older than their
   `idle_timeout` are *not* swept by a background task in v1 — instead
   the next event from that sender sees the expired row and triggers a
   fresh Spawn. Out-of-band sweep can be added with a periodic task in
   the dispatcher if storage growth becomes a concern.
