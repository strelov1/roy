# Connection Catalog — Design

**Date:** 2026-05-27
**Status:** Approved (pending user sign-off)
**Builds on:** `docs/superpowers/plans/2026-05-27-connections-mcp-proxy.md` (already merged backend MVP)

## Problem

The current MVP exposes a generic free-form CRUD for MCP "connections": user types in a name, command, args, env, secrets. This is honest but raw — users must already know which npm package to install, which env var holds the API key, and how to format args. The UX target is Claude.ai's "Connectors" pattern: a curated catalog where the user clicks "Connect", pastes the relevant secret, and is done.

## Decision

Introduce a **provider catalog** as a sibling concept to existing connections. Catalog lives in YAML on the user's machine; connections (instances) stay in SQLite with a new `provider_id` column pointing into the catalog. Single user-facing term: **Connection**. Catalog is the "what we can connect to", a row in `connections` is "what I have connected".

## YAML schema

File: `~/.roy/connections.yaml`. Read on every `GET /providers` (no caching for MVP; the file is small and rarely changed).

```yaml
- id: github                                       # stable string, used as FK from `connections.provider_id`
  name: GitHub                                     # display name
  description: Read/write GitHub repos, issues, PRs
  icon: github                                     # opaque key, looked up by roy-web in its icon table
  command: npx
  args: ['-y', '@modelcontextprotocol/server-github']
  env: {}                                          # optional, static env merged into spawn (not secrets)
  secrets:
    - key: GITHUB_PERSONAL_ACCESS_TOKEN
      label: Personal Access Token
      help: 'github.com/settings/tokens — scope: repo'
```

A sample file shipping with the same shape lives at `crates/roy-management/resources/connections.default.yaml`. The MVP version contains exactly one entry: GitHub.

## Boot policy

- **Missing yaml** (`~/.roy/connections.yaml` does not exist) → empty catalog, management boots normally. Users who don't use MCP connections never need to think about the file.
- **Broken yaml** (file exists but fails to parse or fails schema validation) → management refuses to boot with `Error: ~/.roy/connections.yaml is malformed: <reason>. Fix the file or remove it to use an empty catalog. Sample: <packaged-resource-path>.` Exit 1.
- **Empty yaml** (`[]`) → empty catalog, management boots. Identical to missing.

## DB schema delta

One migration `0007_connections_provider_id.sql` on top of `0006_connections.sql`:

```sql
ALTER TABLE connections ADD COLUMN provider_id TEXT;
CREATE INDEX connections_provider_idx ON connections(provider_id);

-- Per-owner uniqueness on (provider_id, label) — prevents accidental duplicates
-- like two "GitHub · work" rows. NULL provider_id (legacy/custom) is excluded by
-- the WHERE clause so existing free-form rows aren't constrained.
CREATE UNIQUE INDEX connections_owner_provider_label_unique
  ON connections(owner_id, provider_id, name)
  WHERE provider_id IS NOT NULL;
```

`name` (existing column) now plays the role of "label" for catalog-backed connections (e.g. "work", "personal"). For UI display we render `<provider.name> · <connection.name>`.

`config_json` and `secrets_json` remain. At create time the backend resolves `command`/`args`/`env` from yaml and snapshots them into `config_json`. Future yaml edits do NOT retroactively update existing rows (deliberate — silent re-spawn-time behavior changes would be a footgun).

## API surface

### New: `GET /providers`

Read-only, auth-gated. Returns the parsed yaml.

```ts
type ProviderSecretSchema = {
  key: string;
  label: string;
  help?: string | null;
};

type Provider = {
  id: string;
  name: string;
  description: string;
  icon: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  secrets: ProviderSecretSchema[];
};
```

Response: `Provider[]`. Order matches yaml document order.

### Modified: `POST /connections`

Body becomes a tagged union by presence of `provider_id`:

**Catalog-backed flow:**
```json
{
  "provider_id": "github",
  "name": "work",
  "secrets": {"GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_..."}
}
```
Backend reads yaml, locates the provider by `id`, validates that `secrets` contains all `key`s required by the provider's `secrets` schema (extra keys allowed), composes `kind: "mcp_stdio"` + `config: {command, args, env}` from yaml, and inserts. UNIQUE index enforces no duplicate `(owner_id, provider_id, name)`.

**Legacy/custom flow** (unchanged from existing MVP):
```json
{
  "name": "My MCP server",
  "kind": "mcp_stdio",
  "config": {"command": "npx", "args": ["..."]},
  "secrets": {"TOKEN": "..."}
}
```
No `provider_id`. UI does not surface this flow in MVP (one "+ Add custom MCP server" overflow item kept for power users, hidden behind a button — out of MVP scope, may be added later).

Conflict response (`409`) when uniqueness violates: `{"error": "connection already exists: provider 'github' label 'work'"}`.

### Modified: `GET /connections`

Each row now includes `provider_id: string | null` so the UI can join with the catalog client-side.

### Modified: `PUT /connections/{id}`

Accepts `name` and `secrets` updates. `provider_id`, `kind`, and `config` are NOT mutable post-creation (a different connection-type is a different connection — delete + recreate).

### Unchanged: `DELETE /connections/{id}`, wire to daemon, `ConnectionSpec` shape

The daemon never sees `provider_id`. The wire stays `{id, slug, kind, config, secrets}` — the catalog is purely an HTTP-layer/UX concept.

## UI

Page `/connections` (URL and page name unchanged from current `roy-web` PR #21).

### Layout

Two-pane:

- **Left pane (~280px):** scrollable list with two sections:
  - **Connected** — list of `connections.list` grouped by `provider_id`. Each provider with at least one instance shows up here with a checkmark; clicking expands to show its instances by `name`.
  - **Available** — providers from the catalog that have zero instances. Clicking shows the detail in the right pane.
  - Top button: search input (filters both sections).
- **Right pane:** details of the selected provider:
  - Header: icon + name + description.
  - Instances list: for each existing instance, a row with `name` + Disconnect button.
  - `+ Connect` button (opens dialog).
- **Dialog "Connect <provider>":** input for `name` ("work", "personal") + one input per provider's `secrets[]` schema entry (with `label` and `help` rendered as field label and hint). On submit → POST /connections. Errors surface inline (e.g. 409 duplicate).

### What goes away from PR #21

- The carded grid of all connections becomes the left-pane list.
- The current Create/Edit dialog with command/args/env/secrets fields is replaced by the per-provider Connect dialog (only label + provider-defined secret inputs).
- The "+ New connection" button → "+ Add custom MCP server" overflow (deferred; not in MVP).

### Composer picker (existing `ConnectionPicker`)

Unchanged shape but each row now displays `<provider.name> · <connection.name>` when `provider_id` is set, otherwise `<connection.name>` (custom row). Reads from the same `connectionsStore.list`.

## Default catalog

Single entry, GitHub. Other providers (Linear, Notion, Slack, Filesystem) deliberately deferred — adding them is one PR per provider with one yaml entry + one icon. Order of operations:

1. Land this design + GitHub end-to-end.
2. Validate the flow works in production for one provider.
3. Add others one at a time.

## Open questions resolved

- **Catalog location:** `~/.roy/connections.yaml`, user-owned.
- **Concept naming:** single word "Connection" everywhere user-facing. The yaml is internally called the "catalog" but never appears as a label in the UI.
- **Multi-instance:** allowed; unique by `(owner_id, provider_id, name)`.
- **Custom MCP form:** out of MVP UI; legacy POST shape stays valid so existing tests and the CLI path keep working.

## Test plan

Backend (`roy-management`):
- yaml loader: missing file → empty list; valid file → parsed list; malformed yaml → `ConfigError`; schema missing required field → `ConfigError`.
- `POST /connections` catalog-backed: happy path, unknown `provider_id` → 400, missing required secret → 400, duplicate `(provider_id, name)` → 409.
- `POST /connections` legacy: unchanged tests pass.
- UNIQUE index migration applies cleanly with existing rows (none have `provider_id` yet, so the partial index admits everything).

Frontend (`roy-web`):
- `providers` namespace in `management-client.ts` returns the typed catalog.
- `ConnectionsView` two-pane layout renders connected + available correctly.
- Connect dialog validates required fields client-side (don't submit until all `secrets[]` filled).
- 409 surfaces as a usable inline error ("This label already exists for GitHub. Pick a different name.").

End-to-end:
- One existing test in `tests/connections_http.rs` updated to exercise the catalog-backed POST.

## Out of scope

- HTTP / SSE upstream MCP transports — still follow-up from the original plan.
- OAuth — still follow-up.
- "Add custom MCP server" UI button — deferred until users actually ask for it.
- Catalog management UI (editing yaml from inside roy-web) — yaml is hand-edited, full stop.
- Encryption of `secrets_json` at rest — separate follow-up.
- Provider icons beyond GitHub — added one by one as providers come online.

## Architecture diagram

```
┌──────────────────────────────────────────────────────────────────────┐
│ roy-web                                                              │
│  /connections page                                                   │
│   ├─ GET /providers        (catalog from yaml)                       │
│   ├─ GET /connections      (instances from DB)                       │
│   └─ POST /connections     ({provider_id, name, secrets})            │
└──────────────────────────────────────────────────────────────────────┘
                                  │ HTTP
┌──────────────────────────────────────────────────────────────────────┐
│ roy-management                                                       │
│   ┌─ provider_catalog::load()  reads ~/.roy/connections.yaml        │
│   └─ connections::create(provider_id, name, secrets)                 │
│       └─ resolves command/args from catalog, INSERT connections row  │
│          with provider_id snapshotted alongside config_json          │
└──────────────────────────────────────────────────────────────────────┘
                                  │ ClientCommand::Spawn (unchanged)
┌──────────────────────────────────────────────────────────────────────┐
│ roy (daemon) — knows nothing of providers; consumes ConnectionSpec   │
│ as before.                                                           │
└──────────────────────────────────────────────────────────────────────┘
```
