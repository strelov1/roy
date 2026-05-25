# roy-management

HTTP service that owns the canonical agent store (via the shared `roy-agents` library) and starts sessions on the roy daemon by passing the persona inline.

## What it does

- Stores agents in a SQLite DB at `~/.local/state/roy/agents.db` (or `$ROY_AGENTS_DB`). The schema is shared with the future scheduler migration via the `roy-agents` library crate.
- Exposes a small JSON HTTP API (axum) for CRUD + launch.
- Calls the daemon over its Unix socket using `ClientCommand::Spawn` with `system_prompt = agent.prompt`. **The daemon never reaches into this crate.** Same boundary rule as `roy-scheduler` and `roy-gateway`: only `roy`'s wire types cross the line, and only outbound over the socket.

## HTTP API

| Method | Path | Body | Returns |
|--------|------|------|---------|
| `GET`    | `/agents`            | —                | `[Agent]` |
| `POST`   | `/agents`            | `NewAgent`       | `Agent` (201); 400 if `preset` isn't one of `claude`/`gemini`/`opencode`/`codex` |
| `GET`    | `/agents/{id}`       | —                | `Agent` (200/404) |
| `PUT`    | `/agents/{id}`       | `AgentUpdate`    | `Agent` (200/404); 400 on bad preset |
| `DELETE` | `/agents/{id}`       | —                | 204 / 404 |
| `GET`    | `/presets`           | —                | Daemon catalog JSON (`AgentsList` event); 502 if the daemon is down |
| `POST`   | `/agents/{id}/run`   | —                | `{"session": "...", "agent_id": "..."}`; 404 if agent missing, 502 on daemon error |
| `POST`   | `/agents/_builder`   | `{existing_id?}` | `{"agent_id": "...", "session_id": "..."}` — creates a stub agent (when no body) or reuses `existing_id`; spawns a builder session bound to the target for conversational editing |

### Agent shape

```json
{
  "id": "uuid",
  "name": "Strict Reviewer",
  "slug": "strict-reviewer",
  "description": "...",
  "preset": "claude",
  "model": "claude-opus-4-7",
  "prompt": "You are a meticulous reviewer ...",
  "task": null,
  "persistent": false,
  "created_at": "2026-05-25T...",
  "updated_at": "2026-05-25T..."
}
```

`prompt` is the persona/system prompt used for interactive runs. `task` is the standing instruction for scheduled fires (populated once the scheduler migrates onto `roy-agents`).

## Config

| Flag / env var | Default |
|---|---|
| `--addr` / `ROY_MANAGEMENT_ADDR` | `127.0.0.1:8079` |
| `--db` / `ROY_AGENTS_DB`         | `~/.local/state/roy/agents.db` |
| `--socket` / `ROY_SOCKET`        | `~/.roy/daemon.sock` (matches `roy-scheduler`'s default) |

## Boundary

`roy-management` depends on `roy` only for wire types (`ClientCommand`, `ServerEvent`) and on `roy-agents` for the store. It never imports `SessionManager`, `Engine`, `Journal`, or any internal-only API.
