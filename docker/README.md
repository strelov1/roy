# docker

Docker bundle that boots `roy` (Rust daemon + management API + WS gateway)
and `roy-web` (Svelte SPA on nginx) with a single `docker compose up`.

Lives in-tree under `docker/`; the build context is the monorepo root
(`roy/`), with the Rust workspace at the root and the SPA under `workspace/`.

## Container layout

| service          | image           | command                                       | host port |
|------------------|-----------------|-----------------------------------------------|-----------|
| `roy-daemon`     | `roy:local`     | `roy serve`                                   | —         |
| `roy-management` | `roy:local`     | `roy management --addr 0.0.0.0:8079`          | `8079`    |
| `roy-gateway`    | `roy:local`     | `roy gateway --config /etc/roy/gateway.toml`  | `8788`    |
| `roy-web`        | `roy-web:local` | `nginx`                                       | `8080`    |

The three `roy:local` containers share state through three named volumes:

- `roy-home` → `/home/roy/.roy/` (daemon Unix socket, per-session workspace)
- `roy-state` → `/home/roy/.local/state/roy/` (`sessions.db`, `agents.db`)
- `roy-config` → `/home/roy/.config/roy/` (`harnesses.toml`, custom `gateway.toml` override if any)

## Quick start

```bash
cd roy/docker   # the docker bundle lives in-tree

# 1. Generate a JWT secret and put it in .env
cp .env.example .env
echo "ROY_JWT_SECRET=$(openssl rand -hex 32)" >> .env   # or paste manually

# 2. Build and bring everything up (first Rust build takes 5–10 min)
docker compose up -d --build

# 3. Grab the generated bootstrap password (if ROY_BOOTSTRAP_PASSWORD is empty)
docker compose logs roy-management | grep -i bootstrap

# 4. Open the UI
open http://localhost:8080
```

In the UI, sign in as `root` with the password from the logs (or whatever
you set in `ROY_BOOTSTRAP_PASSWORD`).

## Traffic flow

- Browser → `http://localhost:8080` → nginx (the `roy-web` container)
- nginx forwards `/management/*` → `roy-management:8079` (over the docker network)
- Browser opens a WS to `ws://localhost:8788` → `roy-gateway` → daemon Unix socket
- `roy-management` and `roy-gateway` reach `roy-daemon` over the shared
  `roy-home` volume (socket at `/home/roy/.roy/daemon.sock`)

## Harnesses

The image installs npm packages for every known harness (the ACP-adapter
binaries roy spawns):

- `claude-code-acp` (harness `claude`)
- `pi-acp` (harness `pi`)
- `opencode-ai` (harness `opencode`)
- `@google/gemini-cli` (harness `gemini`)
- `codex-acp` (harness `codex`)

If a package gets renamed or doesn't exist, the install command won't fail
the build (see the `|| true` fallback in `Dockerfile.roy`), but the
corresponding harness won't work. Fix the names in `Dockerfile.roy`.

### How to pass keys / credentials to agents

Three working approaches, you can mix them.

**Option 1 — bind-mount host tokens (default).** If you're already logged
into the CLIs on your host (`claude login`, `gemini auth`, …), the compose
file already bind-mounts:

```yaml
- ${HOME}/.claude:/home/roy/.claude:ro
- ${HOME}/.gemini:/home/roy/.gemini:ro
- ${HOME}/.codex:/home/roy/.codex:ro
- ${HOME}/.config/opencode:/home/roy/.config/opencode:ro
```

Comment out the lines you don't need — otherwise the bind fails when the
host directory is missing. If a CLI needs to refresh tokens, drop the `:ro`.

**Option 2 — keys / tokens via `.env`.** Uncomment what you need in `.env`:

```env
ANTHROPIC_API_KEY=sk-ant-...
CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat-...   # Pro/Max subscription instead of API billing
GEMINI_API_KEY=...
OPENAI_API_KEY=sk-...
OPENROUTER_API_KEY=...
```

`docker-compose.yml` forwards these into `roy-daemon`. The daemon spawns
child ACP processes via the standard `Command::spawn` path; env is
inherited automatically. An agent CLI that wants a `*_API_KEY` env var
gets it directly — no OAuth session under `~/.claude` required.

`CLAUDE_CODE_OAUTH_TOKEN` is especially convenient for headless setups:
generated once by `claude setup-token` on a Pro/Max account, long-lived,
and removes the need to bind-mount `~/.claude` at all.

**Option 3 — copy a token into the volume manually.** When bind-mount is
awkward (token stored outside a dot-dir, or behind Keychain), after the
first `up -d`:

```bash
docker compose cp ~/.claude roy-daemon:/home/roy/.claude
docker compose restart roy-daemon
```

The files move into the named `roy-home` volume and survive container
restarts.

**Which to pick.** If the CLI supports a `*_API_KEY` env var, that's the
cleanest path (`.env` is gitignored, nothing is bind-mounted). If it only
supports OAuth, use a bind-mount or a copy. `claude-code-acp`
historically prefers the OAuth session, while `gemini` / `opencode`
usually accept both.

## Useful commands

```bash
# Tail logs for everything
docker compose logs -f

# Open a shell in the running daemon
docker compose exec roy-daemon bash

# Drive an ACP session by hand (daemon must be running)
docker compose exec roy-daemon roy run claude "hello, how are you?"

# Daemon health probe (exit 0 if alive)
docker compose exec roy-daemon roy status

# Nuke all state (DBs, sessions, socket)
docker compose down -v
```

## What to customise

- **Want the Telegram adapter?** Add `[telegram]` + `[binder]` sections to
  `gateway.toml`, mount it via volume
  (`./gateway.toml:/etc/roy/gateway.toml:ro`), and keep `gateway.toml`
  alongside the compose file.
- **Want a custom `harnesses.toml`?** Drop one into
  `~/.config/roy/harnesses.toml` via a volume, e.g.
  `./harnesses.toml:/home/roy/.config/roy/harnesses.toml:ro`.
- **Want a different WS URL for the frontend?** Change `VITE_ROY_WS_URL`
  in `.env` and rebuild the web image only:
  `docker compose build roy-web && docker compose up -d roy-web`.

## Limitations

- ACP agents are external npm CLIs that usually require an interactive
  OAuth login. You can't run that login inside a headless container —
  the token has to live on the host (Option 1) or be supplied via env
  (Option 2).
- The first `cargo build --release` compiles the whole workspace
  (~8 crates) from scratch. Expect 5–10 minutes on first build.
- `Cargo.lock` is listed in `.gitignore` but the file is actually
  present in the repo. If it ever goes missing, run
  `cargo generate-lockfile` at the repo root before building.
