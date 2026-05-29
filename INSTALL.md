# Installing & running roy

This monorepo holds the whole stack:

- `crates/` — the Rust workspace: the daemon and every adapter, all built into a single `roy` binary.
- `workspace/` — the Svelte SPA front-end (chat UI).
- `docker/` — a container bundle that wires it all together.

There are two ways to run it: **Docker** (one command, recommended for a
first run or a server) and **native** (build the binary yourself, best for
hacking on the code).

## Services & ports

| Service          | Run with                                   | Listens on                       |
|------------------|--------------------------------------------|----------------------------------|
| Daemon           | `roy serve`                                | Unix socket `~/.roy/daemon.sock` |
| Management API   | `roy management`                           | `127.0.0.1:8079` (HTTP)          |
| WS gateway       | `roy gateway --config <file>`              | `:8788` (WebSocket relay)        |
| Web SPA (dev)    | `npm run dev` in `workspace/`              | `127.0.0.1:5173` (Vite)          |
| Web SPA (Docker) | nginx in the `roy-web` container           | `:8080`                          |

The daemon is the only thing that talks to harnesses; management, gateway,
and the SPA are clients. The gateway is what the browser connects to — it
authenticates the WebSocket with the same JWT the management API issues.

## Prerequisites

**Harness binaries** are never bundled — roy spawns whichever ACP-adapter
CLIs you have on `PATH`, already authenticated. Install at least one:

| Harness    | Install                                      | Auth                          |
|------------|----------------------------------------------|-------------------------------|
| `claude`   | `npm i -g @zed-industries/claude-code-acp`   | `claude login` or API key     |
| `gemini`   | `npm i -g @google/gemini-cli`                | `gemini` (interactive login)  |
| `opencode` | OpenCode CLI on `PATH`                        | its own login                 |
| `codex`    | `npm i -g @zed-industries/codex-acp`         | API key                       |
| `pi`       | `npm i -g pi-acp`                            | its own login                 |

For the **native** path you also need a Rust toolchain (1.95+) and Node 24+.
The **Docker** path needs only Docker with Compose.

---

## Path A — Docker (recommended)

Everything (daemon, management, gateway, web) comes up with one command.

```bash
cd docker

# 1. Configure. ROY_JWT_SECRET is required (>=32 bytes).
cp .env.example .env
echo "ROY_JWT_SECRET=$(openssl rand -hex 32)" >> .env

# 2. Build and start. First Rust build takes 5-10 min.
docker compose up -d --build

# 3. Grab the auto-generated bootstrap password (first launch only).
docker compose logs roy-management | grep -i bootstrap

# 4. Open the UI and sign in as `root` with that password.
open http://localhost:8080
```

Give agents credentials in one of two ways (see `docker/README.md` for the
full rundown):

- **Bind-mounted host logins** (default): if you've run `claude login`,
  `gemini auth`, etc. on the host, the compose file already mounts
  `~/.claude`, `~/.gemini`, `~/.codex`, `~/.config/opencode`.
- **Env keys**: uncomment `ANTHROPIC_API_KEY` / `CLAUDE_CODE_OAUTH_TOKEN` /
  `GEMINI_API_KEY` / `OPENAI_API_KEY` / `OPENROUTER_API_KEY` in `.env`.

Useful commands:

```bash
docker compose logs -f                 # tail everything
docker compose exec roy-daemon roy status   # health probe
docker compose down                    # stop
docker compose down -v                 # stop + wipe all state (DBs, sessions)
```

`docker-compose.prod.yml` is for deployments off pre-built `ghcr.io` images;
the `docker/scripts/deploy.sh` script builds and rolls them out.

---

## Path B — Native (local development)

Build the binary once, then run the four pieces in separate terminals.

### 1. Build & install the binary

```bash
cargo build --release          # produces target/release/roy
export PATH="$PWD/target/release:$PATH"   # or copy/symlink it onto $PATH
```

### 2. Pick a shared JWT secret

Both the management API and the gateway verify the same token, so export it
in every shell that runs them:

```bash
export ROY_JWT_SECRET="$(openssl rand -hex 32)"
```

### 3. Daemon — terminal 1

```bash
roy serve                      # listens on ~/.roy/daemon.sock
```

### 4. Management API — terminal 2

```bash
export ROY_JWT_SECRET="<same value as above>"
roy management                 # 127.0.0.1:8079
```

On the first run with an empty user table it creates a bootstrap user
(`root` by default) and prints a generated password to stderr **once** —
copy it. Override with `ROY_BOOTSTRAP_USERNAME` / `ROY_BOOTSTRAP_PASSWORD`.

### 5. WS gateway — terminal 3

The gateway needs a config file. Create one that binds `:8788` (the port the
SPA expects):

```bash
cat > gateway.toml <<'EOF'
[websocket]
bind = "127.0.0.1:8788"
EOF

export ROY_JWT_SECRET="<same value as above>"
roy gateway --config gateway.toml
```

(The built-in default bind is `127.0.0.1:8787`; we use `8788` to match the
SPA's `VITE_ROY_WS_URL`.)

### 6. Web SPA — terminal 4

```bash
cd workspace
cp .env.example .env           # VITE_ROY_WS_URL=ws://127.0.0.1:8788
npm install
npm run dev                    # http://127.0.0.1:5173
```

Vite proxies `/management/*` to `http://127.0.0.1:8079`, so the browser
reaches the management API without CORS. Open <http://127.0.0.1:5173> and
sign in with the bootstrap credentials from step 4.

---

## Configuration reference

### Environment variables

| Variable                  | Used by              | Meaning                                                                 |
|---------------------------|----------------------|-------------------------------------------------------------------------|
| `ROY_JWT_SECRET`          | management, gateway  | **Required.** ≥32 ASCII bytes. Signs/verifies the auth JWT.             |
| `ROY_BOOTSTRAP_USERNAME`  | management           | First-user name (default `root`). First launch only.                    |
| `ROY_BOOTSTRAP_PASSWORD`  | management           | First-user password (≥8 chars). If unset, one is generated and logged.  |
| `ROY_MANAGEMENT_ADDR`     | management           | HTTP bind address (default `127.0.0.1:8079`). Same as `--addr`.         |
| `ROY_SOCKET`              | daemon + clients     | Daemon Unix socket path (default `~/.roy/daemon.sock`).                 |
| `ROY_CWD`                 | daemon               | Default project cwd when no client supplies one.                        |
| `ROY_WORKSPACE_DIR`       | management           | Root for per-user/team session dirs (default `~/.roy/workspace`).       |
| `ROY_HARNESSES_CONFIG`    | daemon               | Path to `harnesses.toml` (default `~/.config/roy/harnesses.toml`).      |
| `ROY_AGENTS_DB`           | management           | Path to `agents.db` (default `~/.local/state/roy/agents.db`).           |
| `ANTHROPIC_API_KEY`, …    | spawned agents       | Forwarded to harness child processes that read a key from env.          |

### Config files

| File                                   | Owner       | Purpose                                                        |
|----------------------------------------|-------------|----------------------------------------------------------------|
| `~/.config/roy/harnesses.toml`         | you         | Which harnesses/models are surfaced (see `docs/harnesses-config.md`). |
| `gateway.toml` (path you choose)       | you         | Gateway adapters: `[websocket]` and/or `[telegram]`.           |
| `.roy/agents/<slug>.md`                | you         | Agent personas (YAML frontmatter + system-prompt body).        |
| `workspace/.env`                       | you (dev)   | `VITE_ROY_WS_URL` for the SPA dev server.                      |
| `docker/.env`                          | you (Docker)| `ROY_JWT_SECRET`, bootstrap, agent keys, `VITE_ROY_WS_URL`.    |

### State (SQLite, created automatically)

- `~/.local/state/roy/sessions.db` — session boot-kit metadata (daemon).
- `~/.local/state/roy/agents.db` — projects, agents, users, teams, connections (management + auth).
- `~/.local/state/roy-scheduler/state.db` — scheduler agents/triggers/fires.
- `~/.local/state/roy-inbound/state.db` — inbound channel bindings.

---

## Verify it works

```bash
roy status                                 # exit 0 if the daemon is up
roy run claude "say hi in one word"        # one-shot session, streams JSON events
```

For the CLI surface and library API, see the root `README.md`; for deeper
design notes, see `docs/architecture.md`, `docs/wire-protocol.md`, and
`docs/persistence.md`.
