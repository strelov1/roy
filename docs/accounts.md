# User accounts & authentication

roy is **admin-provisioned, not self-service**: there is no public sign-up
endpoint. Accounts are created from the command line by someone with access
to the `agents.db` file; everyone else signs in with credentials they were
given.

This document covers creating accounts, signing in, and team membership.
For where the data lives, see [`persistence.md`](./persistence.md); for the
services and ports, see [`../INSTALL.md`](../INSTALL.md).

## How it works

- **Storage.** Users, teams, members, and invites live in the shared
  `agents.db` SQLite file (`~/.local/state/roy/agents.db`), owned by the
  `roy-auth` crate. The management API (`roy management`) is the HTTP front.
- **Sessions.** Sign-in is JWT-based. `POST /auth/login` verifies the
  password and sets an `HttpOnly` cookie; the same response carries a
  `ws_token` the browser feeds into the WebSocket subprotocol so the gateway
  can authenticate the socket. Both the API and the gateway verify the token
  with the **same `ROY_JWT_SECRET`** (≥32 ASCII bytes).
- **Who can create users.** Anyone who can read/write `agents.db` can create
  or reset users — that file access *is* the admin credential. The
  `roy auth create` / `roy auth reset` commands talk to the DB directly and
  bypass the HTTP layer entirely (no login, no running server required).

## Creating accounts

### 1. The first user (bootstrap)

On the very first start with an empty `users` table, `roy management`
creates one user automatically:

- username from `ROY_BOOTSTRAP_USERNAME` (default `root`);
- password from `ROY_BOOTSTRAP_PASSWORD`, **or** a random 32-char hex string
  printed to stderr exactly once if that variable is unset.

```bash
# Native: read the generated password from the management process's stderr.
ROY_JWT_SECRET=... roy management
#   roy: bootstrap user "root" — password: 9f3c...

# Docker: read it from the container logs.
docker compose logs roy-management | grep -i bootstrap
```

Set `ROY_BOOTSTRAP_PASSWORD` (≥8 chars) ahead of time to choose it yourself.

### 2. Additional users (`roy auth create`)

Provision more users directly against `agents.db`. No server or login is
needed — local DB access is the credential.

```bash
# Interactive password prompt (recommended — nothing lands in shell history
# or `ps`).
roy auth create alice
roy auth create alice --display-name "Alice Smith"

# Scripted: pipe the password on stdin …
echo 'correct-horse-battery' | roy auth create alice

# … or via env (not visible in `ps`).
ROY_NEW_PASSWORD='correct-horse-battery' roy auth create alice
```

Password resolution order: `--password` flag → `ROY_NEW_PASSWORD` → piped
stdin → interactive prompt. Minimum 8 characters. Avoid `--password` outside
throwaway setups — it is visible in `ps`.

**In Docker**, run it inside a container that mounts the state volume:

```bash
docker compose exec roy-management roy auth create alice
```

### 3. Resetting a password (recovery)

The escape hatch when no one can sign in. Same DB-direct model.

```bash
roy auth reset alice                      # prompts for the new password
echo 'new-password' | roy auth reset alice
docker compose exec roy-management roy auth reset root   # Docker
```

## Signing in

### Web UI

Open the SPA and use the sign-in form — it POSTs to `/auth/login` and the
browser holds the session cookie. Bootstrap/`root` signs in the same way.

### CLI

```bash
roy auth login     # prompts for username + password, saves the cookie
roy auth whoami    # prints the current user (GET /auth/me)
```

The cookie is written to `~/.config/roy/cookie` (mode `0600`). Both commands
target `$ROY_MANAGEMENT_URL` (default `http://127.0.0.1:8079`); override with
`--api http://host:port`.

## Teams & invites

Invites add an **existing** user to a team — they do *not* create accounts.
Both endpoints require an authenticated caller.

| Action            | Endpoint                          | Body                              |
|-------------------|-----------------------------------|-----------------------------------|
| Create a team     | `POST /teams`                     | `{ "name", "description"? }`      |
| Delete a team     | `DELETE /teams/{id}`              | —                                 |
| List my teams     | `GET /teams`                      | —                                 |
| Invite to a team  | `POST /auth/invites`              | `{ "team_id", "expires_at"? }`    |
| Accept an invite  | `POST /auth/accept-invite`        | `{ "token" }`                     |

Typical flow:

1. A team admin calls `POST /auth/invites` for their team and gets back an
   invite token.
2. That token is handed to a user who **already has an account** (created via
   `roy auth create`).
3. The recipient signs in and calls `POST /auth/accept-invite` with the
   token; they are added to the team. Creating the invite requires team-admin
   rights (enforced by `Acl::can_admin_team`).

Example with the saved cookie:

```bash
COOKIE=$(cat ~/.config/roy/cookie)
API=http://127.0.0.1:8079

# Create a team.
curl -s -X POST "$API/teams" -H "cookie: $COOKIE" \
  -H 'content-type: application/json' \
  -d '{"name":"Engineering"}'

# Invite to it (team_id from the previous response).
curl -s -X POST "$API/auth/invites" -H "cookie: $COOKIE" \
  -H 'content-type: application/json' \
  -d '{"team_id":"<team-id>"}'

# The invited (logged-in) user redeems the token.
curl -s -X POST "$API/auth/accept-invite" -H "cookie: $INVITEE_COOKIE" \
  -H 'content-type: application/json' \
  -d '{"token":"<invite-token>"}'
```

## Security notes

- `ROY_JWT_SECRET` must be ≥32 ASCII bytes and identical for `roy management`
  and `roy gateway`; without it the management API refuses to start and the
  gateway rejects every WebSocket.
- Passwords are stored hashed by `roy-auth`; only the hash is in `agents.db`.
- Because DB-file access equals user-management rights, keep `agents.db`
  (mode `0600`) and any host bind-mounts of `~/.local/state/roy` restricted
  to trusted operators.
