# roy-gateway

Bridges chat platforms ↔ a running `roy serve` daemon. v1 supports
**Telegram only**.

## How it works

1. `roy serve` is running, you have a preset (`claude` / `gemini` /
   `opencode` / `codex`) installed and pre-authenticated, and (if you
   want to scope sessions to a specific working directory) you have a
   roy project pre-created.
2. `roy-gateway` runs as a long-lived process. On every inbound text DM:
   - If the chat is new, `Fire { Spawn { preset, project_id } }` is sent
     to the daemon. The returned `session_id` is bound to `chat_id` in a
     JSON file.
   - If the chat is known, `Fire { Resume { session_id } }` is sent. The
     daemon hands the prompt back through ACP `session/load`.
3. When `FireDone` lands, the assistant's final text is sent to the chat
   as one Telegram message.

Streaming partials, message edits, debouncing, and `/cancel` are deferred
to v2 — see the plan doc at `docs/superpowers/plans/2026-05-23-roy-gateway-telegram.md`.

## Config

```toml
# ~/.config/roy-gateway/telegram.toml — DO NOT COMMIT (contains bot token)

[daemon]
# Optional; falls back to ROY_SOCKET, then ~/.roy/daemon.sock
# socket = "/Users/me/.roy/daemon.sock"

[telegram]
token = "1234567890:AA…"            # from @BotFather
allowed_user_ids = [123456789]      # empty list = allow anyone
preset = "claude"
project_id = "proj-abc"             # optional; daemon default cwd otherwise
turn_timeout_secs = 600

[binder]
path = "/Users/me/.roy/gateway-telegram.json"
```

## Run

```bash
# 1. start the daemon (separately, in its own terminal)
roy serve

# 2. start the gateway
RUST_LOG=roy_gateway=info,info \
  cargo run -p roy-gateway -- --config ~/.config/roy-gateway/telegram.toml
```

## Manual smoke checklist

- [ ] DM your bot. Wait for a reply. Confirm the binder file has one entry.
- [ ] Send a follow-up. Confirm the same `session_id` is reused
      (`jq < ~/.roy/gateway-telegram.json`).
- [ ] Stop the gateway (Ctrl-C). Restart. Send another message. Confirm
      the conversation continues.
- [ ] Stop the daemon. Send a message. Expect a `⚠ …` error reply in
      the chat, gateway keeps running.
- [ ] (If `allowed_user_ids` set) DM from a non-allowlisted account.
      Expect silence and a `rejecting non-allowlisted sender` debug log.
