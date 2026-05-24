# roy-gateway

Bridges chat platforms ↔ a running `roy serve` daemon. v1.1 supports
**Telegram only**.

## How it works (v1.1 streaming)

1. `roy serve` is running. You have a preset (`claude` / `gemini` /
   `opencode` / `codex`) installed and pre-authenticated, and optionally a
   roy project pre-created (referenced by `project_id` in config).
2. `roy-gateway` runs as a long-lived process. On every inbound text DM:
   - Send a `⏳` placeholder message to the chat.
   - Open a daemon connection; `Spawn` (new chat) or `Resume` (known chat)
     to get a `session_id`, bind it to `chat_id` in the JSON binder.
   - `AcquireInput` (holds the daemon's input lease for the turn —
     prerequisite for `/cancel`).
   - `Send` the user's prompt.
   - Stream `Frame` events from the daemon. Each event extends the rendered
     HTML body (thinking → italic, tool calls → `<code>`, assistant text →
     plain). The placeholder is edited every ~1 second to show the latest
     body. At 4000 chars the message is finalized and a new one is started.
   - On terminal `Result`, flush final state, `ReleaseInput`, close the
     connection, remove the cancel-registry entry.
3. `/cancel` (DM) signals the streaming task to send `CancelTurn` to the
   daemon, append a `❎ cancelled by user` line, and finalize. If no turn is
   running, the bot replies "Нечего отменять".

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
# 1. start the daemon (separate terminal)
roy serve

# 2. start the gateway
RUST_LOG=roy_gateway=info,info \
  cargo run -p roy-gateway -- --config ~/.config/roy-gateway/telegram.toml
```

## Manual smoke checklist (v1.1)

- [ ] DM your bot. Confirm `⏳` placeholder appears within a second, then
      gets edited as the agent produces text.
- [ ] Confirm `🧠 thinking:` blocks appear (italic) for AssistantThought
      events.
- [ ] Confirm `🔧 <tool>(<args>)` blocks appear for ToolUse events.
- [ ] Verify the chat shows "typing…" status in the header while the turn
      runs.
- [ ] Send a follow-up to the same chat. Confirm same `session_id` is
      reused (`jq < ~/.roy/gateway-telegram.json`).
- [ ] Trigger a long-running turn. Send `/cancel`. Confirm the streaming
      message gains `❎ cancelled by user` footer within ~1 second and the
      bot replies `❎ cancelled`.
- [ ] Send `/cancel` when no turn is running. Confirm reply
      `Нечего отменять — turn не запущен`.
- [ ] Trigger a long agent response that crosses 4000 chars. Confirm the
      message is finalized at a paragraph boundary and a new message
      continues the body.
- [ ] Configure `turn_timeout_secs` low (e.g. 10) and trigger a turn that
      runs longer. Confirm `⚠ turn timed out` footer appears.
- [ ] Stop the daemon. Send a message. Confirm a `⚠ …` error reply
      appears in the chat; gateway keeps running.
- [ ] (If `allowed_user_ids` set) DM from a non-allowlisted account.
      Expect silence and a `rejecting non-allowlisted sender` debug log.

## Still deferred to later iterations

- Debounce of fast successive messages.
- `Channel` trait + Slack/Discord support.
- Persisting full transcripts in chat after edits (history is in roy journal).
- Inline buttons, attachments, voice.
