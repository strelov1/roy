# roy-web

Lightweight Svelte 5 + Vite chat UI for the [`roy`](../roy) daemon. Single
WebSocket connection, no backend in between.

## Prerequisites

The roy daemon must be running with a WebSocket port exposed:

```bash
# in the roy repo
cargo run -p roy-cli -- serve --port 7777
```

The WS listener is unauthenticated by design — keep it on `127.0.0.1`.

## Setup

```bash
npm install
cp .env.example .env       # adjust VITE_ROY_WS_URL if you didn't pick 7777
npm run dev
```

Open the URL Vite prints (default `http://localhost:5173`).

## What it does

- **Left pane** lists live and archived sessions, pulled via `list` /
  `list_archived` on connect. Clicking a live one attaches + acquires the
  input lease; clicking an archived one resumes it first.
- **Right pane** shows the session's journal entries with simple
  per-`TurnEvent`-type rendering (assistant text, tool use, result, system,
  raw). The composer is enabled only while the input lease is held and no
  turn is in flight.
- **+ new** opens a small form to spawn a session against one of the four
  agent presets (`claude_agent`, `gemini`, `opencode`, `codex`). Leave
  `cwd` empty to let the daemon pick its default (`ROY_CWD` env →
  `current_dir`).

## Architecture

Three layers, each in `src/lib/`:

| file | role |
|---|---|
| `wire.ts` | TypeScript mirror of `ClientCommand` / `ServerEvent` / `TurnEvent`. Single source of truth for what the daemon expects. |
| `client.ts` | `RoyClient` — one WebSocket, a FIFO promise queue (`call`), a fire-and-forget escape hatch (`fire`), and per-session frame subscriptions. |
| `state.svelte.ts` | `AppState` — Svelte 5 reactive state. Drives the UI: session lists, current session, journal entries, input-lease + awaiting-turn flags. |

Components (`SessionList.svelte`, `ChatView.svelte`, `NewSessionForm.svelte`)
read from `app` and call its methods; they hold no business state.

### Why `call` vs `fire`

ACP-over-WS replies arrive in command order, but `Send` is fire-and-forget
on the daemon side — its observable effect is the `Frame` stream that
follows, terminated by a `Result`. `client.call` awaits a typed reply and
rejects on `error`. `client.fire` is used only for `Send` to keep the
promise queue's FIFO matching intact.

## Wire format reference

The canonical reference is `docs/wire-protocol.md` in the roy repo. If the
Rust enums in `crates/roy/src/control.rs` or `crates/roy/src/event.rs`
change, `wire.ts` needs the matching update — there is no codegen yet.

## Caveats

- One open WS connection per page. Reloading drops any input lease held by
  this page (the daemon's `LeasesMap` is per-connection).
- No retry/reconnect logic — if the daemon restarts, refresh the page.
- No auth. Run on `127.0.0.1` only.
