# roy-inbound

In-process event bus that lets external systems (HTTP webhooks today, IMAP /
WhatsApp / Telegram-customer-support later) wake up roy agents.

## Quick start

```bash
# 1. Make sure `roy serve` is running.
cargo run -p roy --bin roy -- serve &

# 2. Make sure an agent exists in roy-agents.
roy agents create --name order-bot --preset claude --prompt "You triage orders."

# 3. Write the inbound config.
cat > ~/.config/roy/inbound.toml <<'EOF'
[server]
bind = "127.0.0.1:8090"

[[sources]]
id = "orders"
kind = "webhook"
agent_id = "order-bot"
session = "ephemeral"
template = "New order: {{payload.body}}"
fire_timeout_secs = 600
  [sources.webhook]
  path = "/webhooks/orders"
  reply_mode = "sync"
EOF

# 4. Start the inbound runner.
roy inbound --config ~/.config/roy/inbound.toml

# 5. POST a test event.
curl -s -X POST http://127.0.0.1:8090/webhooks/orders \
     -H 'content-type: application/json' \
     -d '{"id":42,"item":"book"}'
```

## Architecture

See `docs/superpowers/specs/2026-05-25-inbound-event-bus-design.md`.

## Session strategies

- `ephemeral` — every event spawns a fresh roy session
- `persistent_one` — one session for the whole source (all senders share it)
- `per_sender_sticky` — one session per `(source_id, sender_id)` — needs
  `idle_timeout_secs`

## Webhook auth

Set `secret_env = "SOMENAME"` on the source's `[sources.webhook]` table and
provide the HMAC-SHA256 (hex) signature in the `X-Roy-Signature` header.
The signature must be over the raw request body.

## Telegram support

`roy-inbound` can run Telegram bots that reply to users via an agent persona.
Configuration is managed in `roy-management` (web UI or HTTP API), not in
`inbound.toml`:

1. Create a `telegram_bot` connection in roy-management with the bot token in
   the `bot_token` secret field.
2. Create a channel binding (`POST /channel-bindings`) linking that connection
   to an agent slug and a session strategy (typically `per_sender_sticky` with
   an `idle_timeout_secs`).
3. Run `roy inbound` with `ROY_INTERNAL_TOKEN` set to the same value configured
   in roy-management. If roy-management is not at the default address
   (`http://127.0.0.1:8079`), also set `ROY_MANAGEMENT_URL`.

`roy-inbound` polls `GET /internal/telegram-sources` every 30 s and
reconciles the live bot set — adding, removing, or restarting bots as
bindings change. Each Telegram sender gets their own sticky session with the
bound agent; empty `allowed_user_ids` means the bot is public.
