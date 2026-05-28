# Connection Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-05-27-connection-catalog-design.md`

**Goal:** Add a YAML-defined provider catalog on top of the existing `connections` MVP, so users click "Connect → paste token" instead of typing command/args by hand. Single user-facing word stays "Connection".

**Architecture:** Backend reads `~/.roy/connections.yaml` at startup, exposes `GET /providers`, and accepts an extra `provider_id` flow on `POST /connections` that resolves command/args from the catalog. DB gets one new nullable column `provider_id` + a partial UNIQUE index. Frontend replaces the free-form create dialog with a two-pane "Connectors" page that lists connected vs available providers and a per-provider Connect dialog.

**Tech Stack:** Rust 2021 (sqlx 0.8 SQLite, axum 0.8, tokio, `serde_yaml` v0.9 — new dep). Svelte 5 + Vite (roy-web). Two worktrees / two PRs — backend in `/Users/i_strelov/Projects/roy-connections-mcp` on `feat/connection-catalog`; frontend in `/Users/i_strelov/Projects/roy-web` on a fresh `feat/connection-catalog` branch off `main`.

**Execution order:** Phase A (backend) must land before Phase B (frontend) — the UI calls endpoints that don't exist yet. Each phase ends with `cargo test --workspace --no-fail-fast` (backend) / `npm run check && npm run build` (frontend) green.

---

## File map

### Phase A — backend (`/Users/i_strelov/Projects/roy-connections-mcp`)

**New files:**
- `crates/roy-management/migrations/sqlite/0007_connections_provider_id.sql`
- `crates/roy-management/src/provider_catalog.rs` — yaml loader + `Provider` type + `CatalogError`
- `crates/roy-management/resources/connections.default.yaml` — sample one-entry catalog (GitHub)
- `crates/roy-management/tests/provider_catalog.rs` — yaml loader unit tests
- `crates/roy-management/tests/providers_http.rs` — `GET /providers` integration tests

**Modified files:**
- `crates/roy-management/Cargo.toml` — add `serde_yaml = "0.9"` + bundle `resources/connections.default.yaml` as `include_str!`
- `crates/roy-management/src/lib.rs` — load catalog at boot (fail-fast on broken, empty on missing); add catalog to `AppState`
- `crates/roy-management/src/state.rs` — new `catalog: Arc<provider_catalog::Catalog>` field
- `crates/roy-management/src/connections.rs` — extend `NewConnection` to allow the catalog-backed shape; new `connections::create_from_provider` path; expose `provider_id` on `Connection`; new HTTP handlers; mount `GET /providers`
- `crates/roy-management/tests/connections_http.rs` — extend with catalog-backed POST + 409 duplicate

### Phase B — frontend (`/Users/i_strelov/Projects/roy-web`)

**New files:**
- `src/lib/providers.svelte.ts` — providers store (calls `GET /providers`)
- `src/lib/ConnectDialog.svelte` — per-provider Connect modal (label input + dynamic secrets fields)
- `src/lib/ConnectionsView.svelte` — **full rewrite** (two-pane layout)

**Modified files:**
- `src/lib/management-client.ts` — `Provider` type + `providers` namespace; extend `NewConnection` union with catalog-backed shape
- `src/lib/connections.svelte.ts` — list rows now carry `provider_id`
- `src/lib/ConnectionPicker.svelte` — render `<provider.name> · <connection.name>` when `provider_id` present
- `src/lib/Composer.svelte` — no API change; just verify the picker labels render correctly

---

## Phase A — backend

### Task A1: Add `serde_yaml` dep

**Files:**
- Modify: `crates/roy-management/Cargo.toml`

- [ ] **Step 1: Add the dep**

In `crates/roy-management/Cargo.toml`, add to `[dependencies]` next to the existing `serde_json = "1"`:

```toml
serde_yaml = "0.9"
```

- [ ] **Step 2: Verify it resolves**

Run: `cargo build -p roy-management 2>&1 | tail -3`
Expected: clean build, no errors. (`serde_yaml` 0.9 has no transitive surprises here.)

- [ ] **Step 3: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add crates/roy-management/Cargo.toml
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "build(roy-management): add serde_yaml for provider catalog"
```

### Task A2: Migration `0007_connections_provider_id.sql`

**Files:**
- Create: `crates/roy-management/migrations/sqlite/0007_connections_provider_id.sql`
- Modify: `crates/roy-management/src/meta_store.rs` (one-line test assertion bump — same fix pattern as A1 in the previous plan)

- [ ] **Step 1: Write the migration**

Create `crates/roy-management/migrations/sqlite/0007_connections_provider_id.sql`:

```sql
-- 0007_connections_provider_id.sql
--
-- Wires the YAML provider catalog into the `connections` table.
-- `provider_id` is a string FK by name into `~/.roy/connections.yaml` (the
-- catalog is read-only, lives outside the DB — no real FK constraint to
-- enforce, just a soft reference).
--
-- The partial UNIQUE index enforces "one (provider, label) per owner" for
-- catalog-backed rows. Legacy free-form rows (provider_id IS NULL) are
-- excluded so existing connections aren't constrained.

ALTER TABLE connections ADD COLUMN provider_id TEXT;
CREATE INDEX connections_provider_idx ON connections(provider_id);
CREATE UNIQUE INDEX connections_owner_provider_label_unique
  ON connections(owner_id, provider_id, name)
  WHERE provider_id IS NOT NULL;
```

- [ ] **Step 2: Bump the pinned migration list test**

In `crates/roy-management/src/meta_store.rs` find the assertion (it currently asserts versions `1..=6`). Change `vec![(1,), (2,), (3,), (4,), (5,), (6,)]` to `vec![(1,), (2,), (3,), (4,), (5,), (6,), (7,)]`. Same line edit as the previous plan's A1 fix.

- [ ] **Step 3: Verify**

Run: `cargo test -p roy-management --no-fail-fast 2>&1 | grep -E "test result|FAILED" | tail -10`
Expected: all green; new column applied to every test pool.

- [ ] **Step 4: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add \
  crates/roy-management/migrations/sqlite/0007_connections_provider_id.sql \
  crates/roy-management/src/meta_store.rs
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "feat(roy-management): connections.provider_id + unique (owner, provider, label)"
```

### Task A3: Default catalog file + loader skeleton

**Files:**
- Create: `crates/roy-management/resources/connections.default.yaml`
- Create: `crates/roy-management/src/provider_catalog.rs`
- Modify: `crates/roy-management/src/lib.rs` — declare `pub mod provider_catalog;`

- [ ] **Step 1: Write the default catalog**

Create `crates/roy-management/resources/connections.default.yaml` with this exact content:

```yaml
# Default provider catalog shipped with roy-management. The runtime loader
# reads ~/.roy/connections.yaml (user-owned); this file is a reference only
# and is NOT copied onto users' machines automatically.

- id: github
  name: GitHub
  description: Read/write GitHub repos, issues, PRs
  icon: github
  command: npx
  args:
    - '-y'
    - '@modelcontextprotocol/server-github'
  secrets:
    - key: GITHUB_PERSONAL_ACCESS_TOKEN
      label: Personal Access Token
      help: 'github.com/settings/tokens — scope: repo'
```

- [ ] **Step 2: Write the loader module skeleton**

Create `crates/roy-management/src/provider_catalog.rs`:

```rust
//! User-owned provider catalog. Reads `~/.roy/connections.yaml` once at
//! startup. The HTTP `/providers` endpoint serves the same in-memory copy
//! to every caller (no per-request file I/O).
//!
//! Boot policy:
//! * Missing file → empty catalog. Users who don't use MCP connections
//!   never need to think about the file.
//! * Broken file (exists but malformed) → load returns `Err(CatalogError)`;
//!   `lib.rs::run` propagates this as a fatal startup error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One provider definition from the YAML catalog. Mirrors the spec's schema
/// directly — fields are renamed to match the on-disk format via `serde`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretSchema {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("validation error in {path}: {reason}")]
    Schema { path: PathBuf, reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    providers: Vec<Provider>,
}

impl Catalog {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    pub fn get(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Load from `path`. Missing file → empty catalog. Broken file → Err.
    pub fn load_from(path: &Path) -> Result<Self, CatalogError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(e) => {
                return Err(CatalogError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let providers: Vec<Provider> = serde_yaml::from_str(&text).map_err(|e| {
            CatalogError::Parse {
                path: path.to_path_buf(),
                source: e,
            }
        })?;
        for (i, p) in providers.iter().enumerate() {
            if p.id.is_empty() {
                return Err(CatalogError::Schema {
                    path: path.to_path_buf(),
                    reason: format!("entry #{i}: `id` is empty"),
                });
            }
            if p.command.is_empty() {
                return Err(CatalogError::Schema {
                    path: path.to_path_buf(),
                    reason: format!("entry `{}`: `command` is empty", p.id),
                });
            }
        }
        // Reject duplicate ids — silent overwrite is worse than a startup error.
        let mut seen = std::collections::HashSet::new();
        for p in &providers {
            if !seen.insert(p.id.clone()) {
                return Err(CatalogError::Schema {
                    path: path.to_path_buf(),
                    reason: format!("duplicate provider id `{}`", p.id),
                });
            }
        }
        Ok(Self { providers })
    }
}

/// Default path: `~/.roy/connections.yaml`. Mirrors how the rest of the
/// codebase resolves `~/.roy/*` (via `dirs::home_dir`).
pub fn default_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".roy/connections.yaml")
}

/// The default catalog shipped in the repo (`resources/connections.default.yaml`),
/// available at compile time. Used by tests and as a reference path for the
/// boot-error message.
pub const DEFAULT_CATALOG_YAML: &str =
    include_str!("../resources/connections.default.yaml");
```

In `crates/roy-management/src/lib.rs`, add `pub mod provider_catalog;` alphabetically (after `pub mod orphan_sweep;` or similar).

- [ ] **Step 3: Build to make sure it compiles**

Run: `cargo build -p roy-management 2>&1 | grep -E "warning|error" | head -5`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add \
  crates/roy-management/resources/connections.default.yaml \
  crates/roy-management/src/provider_catalog.rs \
  crates/roy-management/src/lib.rs
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "feat(roy-management): provider_catalog module + default catalog yaml"
```

### Task A4: Loader unit tests

**Files:**
- Create: `crates/roy-management/tests/provider_catalog.rs`

- [ ] **Step 1: Write the tests**

Create `crates/roy-management/tests/provider_catalog.rs`:

```rust
//! Tests for the provider catalog loader. Pure file I/O + serde — no DB.

use roy_management::provider_catalog::{Catalog, CatalogError, DEFAULT_CATALOG_YAML};
use std::io::Write;

fn write_temp(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn missing_file_returns_empty_catalog() {
    let path = std::path::PathBuf::from("/tmp/does-not-exist-xxxxxxxx.yaml");
    let cat = Catalog::load_from(&path).unwrap();
    assert!(cat.providers().is_empty());
}

#[test]
fn empty_yaml_returns_empty_catalog() {
    let f = write_temp("[]\n");
    let cat = Catalog::load_from(f.path()).unwrap();
    assert!(cat.providers().is_empty());
}

#[test]
fn default_catalog_parses_and_contains_github() {
    // Reuses the embedded resource so the test fails if we ever break the
    // shipped sample.
    let providers: Vec<roy_management::provider_catalog::Provider> =
        serde_yaml::from_str(DEFAULT_CATALOG_YAML).unwrap();
    let github = providers.iter().find(|p| p.id == "github").unwrap();
    assert_eq!(github.command, "npx");
    assert_eq!(
        github.secrets[0].key,
        "GITHUB_PERSONAL_ACCESS_TOKEN"
    );
}

#[test]
fn malformed_yaml_returns_parse_error() {
    let f = write_temp("not: valid: yaml: [\n");
    let err = Catalog::load_from(f.path()).unwrap_err();
    assert!(matches!(err, CatalogError::Parse { .. }), "{err}");
}

#[test]
fn missing_required_field_returns_schema_error() {
    // No `command` → serde_yaml deserialization fails before our schema
    // check; that's parse error, not schema.
    let f = write_temp("- id: x\n  name: x\n");
    let err = Catalog::load_from(f.path()).unwrap_err();
    assert!(matches!(err, CatalogError::Parse { .. }), "{err}");
}

#[test]
fn empty_id_returns_schema_error() {
    let f = write_temp("- id: ''\n  name: x\n  command: x\n");
    let err = Catalog::load_from(f.path()).unwrap_err();
    match err {
        CatalogError::Schema { reason, .. } => assert!(reason.contains("`id` is empty"), "{reason}"),
        _ => panic!("expected Schema error, got {err}"),
    }
}

#[test]
fn duplicate_id_returns_schema_error() {
    let f = write_temp(
        "- id: dup\n  name: A\n  command: x\n- id: dup\n  name: B\n  command: y\n",
    );
    let err = Catalog::load_from(f.path()).unwrap_err();
    match err {
        CatalogError::Schema { reason, .. } => assert!(reason.contains("duplicate"), "{reason}"),
        _ => panic!("expected Schema error, got {err}"),
    }
}

#[test]
fn get_by_id_returns_the_right_provider() {
    let f = write_temp(
        "- id: github\n  name: GitHub\n  command: npx\n  args: ['-y', '@x/y']\n",
    );
    let cat = Catalog::load_from(f.path()).unwrap();
    assert_eq!(cat.get("github").unwrap().command, "npx");
    assert!(cat.get("nonexistent").is_none());
}
```

If `tempfile` isn't already in `[dev-dependencies]`, add it.

- [ ] **Step 2: Check dev-dep**

Run: `grep tempfile /Users/i_strelov/Projects/roy-connections-mcp/crates/roy-management/Cargo.toml`
Expected: matches in `[dev-dependencies]`. If missing, add `tempfile = "3"` under `[dev-dependencies]`.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p roy-management --test provider_catalog 2>&1 | tail -15`
Expected: 8 tests pass.

- [ ] **Step 4: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add \
  crates/roy-management/tests/provider_catalog.rs \
  crates/roy-management/Cargo.toml
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "test(roy-management): provider catalog loader unit tests"
```

### Task A5: Wire catalog into boot + AppState

**Files:**
- Modify: `crates/roy-management/src/state.rs`
- Modify: `crates/roy-management/src/lib.rs`
- Modify: `crates/roy-management/tests/common/mod.rs`

- [ ] **Step 1: Add `catalog` to `AppState`**

In `crates/roy-management/src/state.rs`, add the field (after `connections`):

```rust
    /// Read-only provider catalog loaded from `~/.roy/connections.yaml` at
    /// boot. Cloneable because `Arc<Catalog>` is. Empty for users without
    /// a yaml file.
    pub catalog: std::sync::Arc<crate::provider_catalog::Catalog>,
```

- [ ] **Step 2: Load catalog in `lib.rs::run`**

In `crates/roy-management/src/lib.rs`, inside `pub async fn run(args)`, find the line that builds `AppState`. Above it, insert:

```rust
    let catalog_path = crate::provider_catalog::default_path();
    let catalog = match crate::provider_catalog::Catalog::load_from(&catalog_path) {
        Ok(c) => {
            tracing::info!(
                path = %catalog_path.display(),
                providers = c.providers().len(),
                "provider catalog loaded"
            );
            std::sync::Arc::new(c)
        }
        Err(e) => {
            // Fail-fast on a broken yaml. The error message tells the user
            // where the file is and points at the bundled sample.
            anyhow::bail!(
                "provider catalog at {} is malformed: {e}. Fix the file or \
                remove it to use an empty catalog. Reference sample at: \
                crates/roy-management/resources/connections.default.yaml",
                catalog_path.display()
            );
        }
    };
```

Then in the `AppState { ... }` literal add `catalog,` next to `connections`.

- [ ] **Step 3: Update the test harness**

In `crates/roy-management/tests/common/mod.rs`, find the `AppState { ... }` literal in `test_app()`. Add:

```rust
        catalog: std::sync::Arc::new(roy_management::provider_catalog::Catalog::empty()),
```

Same in `test_app_with_mock_daemon()` (the helper added in F1 of the previous plan).

Also update any inline `AppState { ... }` inside `crates/roy-management/src/http.rs` `#[cfg(test)] mod tests` blocks — same field, `Catalog::empty()`.

- [ ] **Step 4: Run workspace tests**

Run: `cargo test -p roy-management --no-fail-fast 2>&1 | grep -E "test result|FAILED" | tail -10`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add \
  crates/roy-management/src/state.rs \
  crates/roy-management/src/lib.rs \
  crates/roy-management/src/http.rs \
  crates/roy-management/tests/common/mod.rs
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "feat(roy-management): load provider catalog at boot; fail-fast on broken yaml"
```

### Task A6: `Connection.provider_id` + DB round-trip

**Files:**
- Modify: `crates/roy-management/src/connections.rs`

- [ ] **Step 1: Add `provider_id` to the `Connection` struct**

In `crates/roy-management/src/connections.rs`, find `pub struct Connection`. Add the new field at the bottom (before the closing `}`):

```rust
    pub provider_id: Option<String>,
```

- [ ] **Step 2: Persist and read it**

Find `Store::create` and the INSERT statement. Update both the column list and the bind chain:

```rust
            let res = sqlx::query(
                "INSERT INTO connections
                 (id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at, provider_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(owner_id)
            .bind(&new.name)
            .bind(&slug)
            .bind(&new.kind)
            .bind(&cfg_text)
            .bind(secrets_text.as_deref())
            .bind(new.description.as_deref())
            .bind(now)
            .bind(now)
            .bind(provider_id.as_deref())
            .execute(&self.pool)
            .await;
```

And accept `provider_id: Option<&str>` as a parameter to `Store::create`. Or — cleaner — extend `NewConnection` with a `provider_id` field (see Task A7 for the wire shape). For now in this step, only add the column wiring; A7 wires it through.

Add `provider_id` to the field list in `Connection { ... }` return value (set to whatever was passed in).

- [ ] **Step 3: Read it in `Store::get` and `Store::list_by_owner`**

Both methods use a `ConnectionRow` typedef (a tuple of column types). Extend the tuple with `Option<String>` for `provider_id`. Add `provider_id` to the SELECT column list. Update `row_to_connection` to set the new field on the returned `Connection`.

- [ ] **Step 4: Run existing tests to confirm round-trip**

Run: `cargo test -p roy-management --lib connections::store_tests 2>&1 | tail -10`
Expected: 3 tests pass (existing `create_list_get_update_delete`, `slug_collisions_get_suffixed`, `one_owner_cannot_see_another_users_connections`). They don't reference `provider_id` yet — they should still pass because the column is nullable.

- [ ] **Step 5: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add crates/roy-management/src/connections.rs
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "feat(roy-management): Connection.provider_id round-trips through SQLite"
```

### Task A7: `POST /connections` catalog-backed flow

**Files:**
- Modify: `crates/roy-management/src/connections.rs`

- [ ] **Step 1: Extend `NewConnection` wire shape**

Replace the existing `pub struct NewConnection` with a tagged union covering both flows. Use serde's untagged enum to avoid a new discriminator field:

```rust
/// Two ways to create a connection:
/// * **Catalog-backed:** `{ provider_id, name, secrets }` — backend resolves
///   command/args/env from the yaml catalog. The dominant flow.
/// * **Legacy/custom:** `{ name, kind, config, secrets }` — free-form.
///   Kept for the existing CLI/test paths; UI no longer exposes it in MVP.
///
/// `serde(untagged)` picks the right variant by which fields the body has.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NewConnection {
    FromProvider(NewConnectionFromProvider),
    Custom(NewConnectionCustom),
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewConnectionFromProvider {
    pub provider_id: String,
    pub name: String,
    #[serde(default)]
    pub secrets: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewConnectionCustom {
    pub name: String,
    pub kind: String,
    pub config: Value,
    #[serde(default)]
    pub secrets: Option<Value>,
    #[serde(default)]
    pub description: Option<String>,
}
```

Update every reference to old `NewConnection` field access in `Store::create` — split into two store methods. Cleaner shape:

```rust
impl Store {
    /// Insert a row built directly from a catalog provider definition.
    pub async fn create_from_provider(
        &self,
        owner_id: &str,
        req: NewConnectionFromProvider,
        provider: &crate::provider_catalog::Provider,
    ) -> Result<Connection, StoreError> {
        // Validate secrets contain all required keys.
        validate_required_secrets(provider, req.secrets.as_ref())
            .map_err(StoreError::Invalid)?;
        let config = serde_json::json!({
            "command": provider.command,
            "args": provider.args,
            "env": provider.env,
        });
        let new = NewConnectionCustom {
            name: req.name,
            kind: super::KIND_MCP_STDIO.to_string(),
            config,
            secrets: req.secrets,
            description: Some(provider.description.clone()).filter(|s| !s.is_empty()),
        };
        self.create_custom_inner(owner_id, new, Some(provider.id.clone())).await
    }

    /// Pre-existing free-form flow. Renamed from `create` to keep the
    /// `provider_id` path obviously distinct.
    pub async fn create_custom(
        &self,
        owner_id: &str,
        req: NewConnectionCustom,
    ) -> Result<Connection, StoreError> {
        self.create_custom_inner(owner_id, req, None).await
    }

    async fn create_custom_inner(
        &self,
        owner_id: &str,
        req: NewConnectionCustom,
        provider_id: Option<String>,
    ) -> Result<Connection, StoreError> {
        // ... existing body of the old `create`, threaded with `provider_id`
        // bind ...
    }
}

fn validate_required_secrets(
    provider: &crate::provider_catalog::Provider,
    supplied: Option<&Value>,
) -> Result<(), String> {
    if provider.secrets.is_empty() {
        return Ok(());
    }
    let supplied_obj = supplied
        .and_then(Value::as_object)
        .ok_or_else(|| {
            format!("secrets must be an object with keys: {}", required_keys(provider))
        })?;
    let mut missing: Vec<&str> = Vec::new();
    for s in &provider.secrets {
        match supplied_obj.get(&s.key) {
            Some(Value::String(v)) if !v.is_empty() => {}
            _ => missing.push(s.key.as_str()),
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing required secrets: {}", missing.join(", ")))
    }
}

fn required_keys(provider: &crate::provider_catalog::Provider) -> String {
    provider
        .secrets
        .iter()
        .map(|s| s.key.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
```

- [ ] **Step 2: Update the HTTP handler**

In `create_handler` (~line 433), dispatch on the enum:

```rust
async fn create_handler(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    Json(body): Json<NewConnection>,
) -> Result<(StatusCode, Json<Connection>), ApiError> {
    let c = match body {
        NewConnection::FromProvider(req) => {
            let provider = s
                .catalog
                .get(&req.provider_id)
                .ok_or_else(|| {
                    ApiError(
                        StatusCode::BAD_REQUEST,
                        format!("unknown provider: {}", req.provider_id),
                    )
                })?
                .clone();
            s.connections
                .create_from_provider(&uid, req, &provider)
                .await
                .map_err(map_store_err)?
        }
        NewConnection::Custom(req) => s
            .connections
            .create_custom(&uid, req)
            .await
            .map_err(map_store_err)?,
    };
    Ok((StatusCode::CREATED, Json(c)))
}

fn map_store_err(e: StoreError) -> ApiError {
    // UNIQUE violation on the new partial index → 409 Conflict with a
    // user-readable message.
    if let StoreError::Db(sqlx::Error::Database(d)) = &e {
        if d.is_unique_violation() {
            return ApiError(
                StatusCode::CONFLICT,
                "a connection with this provider and label already exists".into(),
            );
        }
    }
    e.into()
}
```

- [ ] **Step 3: Update all existing callsites of the old `Store::create`**

Search `grep -rn "connections.create\b\|\.connections\.create(" crates/roy-management/src crates/roy-management/tests`. There's typically:
- `connections.rs` `create_handler` itself — handled above.
- Any unit/integration tests under `connections::store_tests` and `tests/connections_http.rs`.

Replace `store.create(uid, NewConnection { ... })` style with `store.create_custom(uid, NewConnectionCustom { ... })` (free-form rows in legacy tests stay free-form, just renamed).

- [ ] **Step 4: Run all roy-management tests**

Run: `cargo test -p roy-management --no-fail-fast 2>&1 | grep -E "test result|FAILED" | tail -10`
Expected: green except any test that POSTed to `/connections` with the old shape — those need the body switched to `NewConnectionCustom` shape (which is the legacy shape with all four fields, so probably already matches). Fix any breakage.

- [ ] **Step 5: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add crates/roy-management/src/connections.rs
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "feat(roy-management): catalog-backed POST /connections (provider_id + label + secrets)"
```

### Task A8: `GET /providers` endpoint

**Files:**
- Modify: `crates/roy-management/src/connections.rs`

- [ ] **Step 1: Add the handler**

Append in the HTTP section of `crates/roy-management/src/connections.rs`:

```rust
async fn providers_handler(
    axum::extract::Extension(_uid): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
) -> Json<Vec<crate::provider_catalog::Provider>> {
    Json(s.catalog.providers().to_vec())
}
```

Mount it in the same router builder near `route("/connections")`:

```rust
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connections", get(list_handler).post(create_handler))
        .route(
            "/connections/{id}",
            get(get_handler).put(update_handler).delete(delete_handler),
        )
        .route("/providers", get(providers_handler))
}
```

- [ ] **Step 2: Integration test**

Create `crates/roy-management/tests/providers_http.rs`:

```rust
//! `GET /providers` end-to-end via the management test harness.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::{login_as, test_app};

#[tokio::test]
async fn empty_catalog_returns_empty_array() {
    let (app, pool, _ws) = test_app().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn unauthenticated_returns_401() {
    let (app, _pool, _ws) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p roy-management --test providers_http 2>&1 | tail -10`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add \
  crates/roy-management/src/connections.rs \
  crates/roy-management/tests/providers_http.rs
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "feat(roy-management): GET /providers serves the catalog"
```

### Task A9: Integration tests for catalog-backed POST

**Files:**
- Modify: `crates/roy-management/tests/connections_http.rs`
- Modify: `crates/roy-management/tests/common/mod.rs` — new helper `test_app_with_catalog`

- [ ] **Step 1: Add `test_app_with_catalog` helper**

In `crates/roy-management/tests/common/mod.rs`, append (mirroring `test_app_with_mock_daemon` from F1):

```rust
/// Variant of `test_app` whose catalog is pre-loaded with a one-entry GitHub
/// provider — enough to exercise the catalog-backed POST flow without
/// touching the user's real ~/.roy/connections.yaml.
pub async fn test_app_with_catalog() -> (axum::Router, sqlx::SqlitePool, std::path::PathBuf) {
    let github_yaml = "\
- id: github
  name: GitHub
  description: Read/write
  command: npx
  args: ['-y', '@modelcontextprotocol/server-github']
  secrets:
    - key: GITHUB_PERSONAL_ACCESS_TOKEN
      label: Personal Access Token
";
    let providers: Vec<roy_management::provider_catalog::Provider> =
        serde_yaml::from_str(github_yaml).unwrap();
    let catalog = std::sync::Arc::new(roy_management::provider_catalog::Catalog::from_providers(providers));

    // ... rest is a copy of `test_app` with `catalog` overridden ...
}
```

To make this work, add a public constructor on `Catalog` in `provider_catalog.rs`:

```rust
#[cfg(feature = "test-support")]
pub fn from_providers(providers: Vec<Provider>) -> Self {
    Self { providers }
}
```

Gate behind the `test-support` feature so it never leaks into release builds.

(If the existing `test_app()` already takes any optional catalog parameter, just pass the new one through. Otherwise the new helper duplicates `test_app`'s body with one override — small duplication is fine for a test harness.)

- [ ] **Step 2: Write the integration tests**

In `crates/roy-management/tests/connections_http.rs` append:

```rust
#[tokio::test]
async fn create_from_provider_happy_path() {
    let (app, pool, _ws) = common::test_app_with_catalog().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let body = json!({
        "provider_id": "github",
        "name": "work",
        "secrets": {"GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_xxx"}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(created["provider_id"], "github");
    assert_eq!(created["name"], "work");
    assert_eq!(created["config"]["command"], "npx");
}

#[tokio::test]
async fn create_from_provider_unknown_id_returns_400() {
    let (app, pool, _ws) = common::test_app_with_catalog().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let body = json!({"provider_id": "nope", "name": "x", "secrets": {}});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_from_provider_missing_secret_returns_400() {
    let (app, pool, _ws) = common::test_app_with_catalog().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let body = json!({"provider_id": "github", "name": "work", "secrets": {}});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn duplicate_provider_label_returns_409() {
    let (app, pool, _ws) = common::test_app_with_catalog().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let body = json!({
        "provider_id": "github",
        "name": "work",
        "secrets": {"GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_xxx"}
    });

    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::CREATED);

    let resp2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p roy-management --test connections_http 2>&1 | tail -15`
Expected: existing 7 tests + 4 new pass.

- [ ] **Step 4: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-connections-mcp add \
  crates/roy-management/tests/connections_http.rs \
  crates/roy-management/tests/common/mod.rs \
  crates/roy-management/src/provider_catalog.rs
git -C /Users/i_strelov/Projects/roy-connections-mcp commit -m "test(roy-management): /connections catalog-backed POST + 409 duplicate"
```

---

## Phase B — frontend

Open a fresh worktree-or-branch in `/Users/i_strelov/Projects/roy-web` called `feat/connection-catalog`, branching off `main` (NOT off `feat/connections-ui` — Phase B replaces the UI from PR #21, easier from a clean main and force-push later if both PRs land at the same time).

```bash
cd /Users/i_strelov/Projects/roy-web
git checkout main
git pull
git checkout -b feat/connection-catalog
```

### Task B1: `Provider` type + `providers` namespace in management-client

**Files:**
- Modify: `/Users/i_strelov/Projects/roy-web/src/lib/management-client.ts`

- [ ] **Step 1: Add types**

In `management-client.ts`, insert the catalog types after the existing `Connection`/`McpStdioConfig` types:

```ts
export type ProviderSecretSchema = {
  key: string;
  label: string;
  help?: string | null;
};

export type Provider = {
  id: string;
  name: string;
  description: string;
  icon: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  secrets: ProviderSecretSchema[];
};

/** Catalog-backed POST body. Backend resolves command/args/env from yaml. */
export type NewConnectionFromProvider = {
  provider_id: string;
  name: string;
  secrets: Record<string, string>;
};
```

Extend the existing `NewConnection` to allow either flow (kept loose so the legacy CLI tests stay valid):

```ts
export type NewConnection = NewConnectionFromProvider | NewConnectionCustom;

export type NewConnectionCustom = {
  name: string;
  kind: 'mcp_stdio';
  config: McpStdioConfig;
  secrets?: Record<string, string> | null;
  description?: string | null;
};
```

Add `provider_id: string | null` to `Connection`:

```ts
export type Connection = {
  // ... existing fields ...
  provider_id: string | null;
};
```

Add the providers namespace next to `connections`:

```ts
export const providers = {
  list: () => request<Provider[]>('/providers'),
};
```

- [ ] **Step 2: Check the project**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run check 2>&1 | tail -10`
Expected: no new errors. (Some existing UI code may now flag the loose union — adjust call sites in B5 if needed.)

- [ ] **Step 3: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-web add src/lib/management-client.ts
git -C /Users/i_strelov/Projects/roy-web commit -m "feat(client): Provider type + providers namespace"
```

### Task B2: Providers store

**Files:**
- Create: `/Users/i_strelov/Projects/roy-web/src/lib/providers.svelte.ts`

- [ ] **Step 1: Write the store**

```ts
// Read-only providers catalog. Loaded once per session; refresh() forces a reload.

import { providers as api, type Provider } from './management-client';

class ProvidersState {
  list = $state<Provider[]>([]);
  loading = $state(false);
  loaded = $state(false);
  error = $state<string | null>(null);

  async load(force = false): Promise<void> {
    if ((this.loaded || this.loading) && !force) return;
    this.loading = true;
    this.error = null;
    try {
      this.list = await api.list();
      this.loaded = true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  get(id: string): Provider | undefined {
    return this.list.find((p) => p.id === id);
  }
}

export const providersStore = new ProvidersState();
export type { Provider } from './management-client';
```

- [ ] **Step 2: Check**

Run: `cd /Users/i_strelov/Projects/roy-web && npm run check 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-web add src/lib/providers.svelte.ts
git -C /Users/i_strelov/Projects/roy-web commit -m "feat(web): providers store reading GET /providers"
```

### Task B3: ConnectDialog

**Files:**
- Create: `/Users/i_strelov/Projects/roy-web/src/lib/ConnectDialog.svelte`

- [ ] **Step 1: Write the dialog**

```svelte
<script lang="ts">
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import * as Dialog from '$lib/components/ui/dialog';
  import { connectionsStore } from './connections.svelte';
  import type { Provider } from './providers.svelte';

  let {
    provider,
    open = $bindable(false),
    onConnected,
  }: {
    provider: Provider;
    open?: boolean;
    onConnected?: () => void;
  } = $props();

  let label = $state('default');
  let secretValues = $state<Record<string, string>>({});
  let submitting = $state(false);
  let error = $state<string | null>(null);

  // Reset when dialog opens (fresh form per open).
  $effect(() => {
    if (open) {
      label = 'default';
      secretValues = Object.fromEntries(provider.secrets.map((s) => [s.key, '']));
      error = null;
    }
  });

  async function submit() {
    if (submitting) return;
    if (!label.trim()) {
      error = 'Label is required';
      return;
    }
    for (const s of provider.secrets) {
      if (!secretValues[s.key]?.trim()) {
        error = `${s.label} is required`;
        return;
      }
    }
    submitting = true;
    error = null;
    try {
      await connectionsStore.create({
        provider_id: provider.id,
        name: label.trim(),
        secrets: secretValues,
      });
      open = false;
      onConnected?.();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Content class="max-w-md">
    <Dialog.Header>
      <Dialog.Title>Connect {provider.name}</Dialog.Title>
      {#if provider.description}
        <Dialog.Description>{provider.description}</Dialog.Description>
      {/if}
    </Dialog.Header>

    <div class="space-y-4 py-2">
      <div class="space-y-1.5">
        <Label for="conn-label">Label</Label>
        <Input
          id="conn-label"
          bind:value={label}
          placeholder="work, personal, …"
          autocomplete="off"
        />
        <p class="text-xs text-muted-foreground">
          Distinguishes this instance from other {provider.name} connections.
        </p>
      </div>

      {#each provider.secrets as secret (secret.key)}
        <div class="space-y-1.5">
          <Label for={`secret-${secret.key}`}>{secret.label}</Label>
          <Input
            id={`secret-${secret.key}`}
            type="password"
            bind:value={secretValues[secret.key]}
            autocomplete="off"
          />
          {#if secret.help}
            <p class="text-xs text-muted-foreground">{secret.help}</p>
          {/if}
        </div>
      {/each}

      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
    </div>

    <Dialog.Footer>
      <Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
      <Button onclick={submit} disabled={submitting}>
        {submitting ? 'Connecting…' : 'Connect'}
      </Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
```

- [ ] **Step 2: Check**

Run: `npm run check 2>&1 | tail -5`
Expected: clean. The `connectionsStore.create` shape is broad enough to accept the catalog payload from B1.

- [ ] **Step 3: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-web add src/lib/ConnectDialog.svelte
git -C /Users/i_strelov/Projects/roy-web commit -m "feat(web): per-provider Connect dialog"
```

### Task B4: ConnectionsView rewrite

**Files:**
- Modify (full rewrite): `/Users/i_strelov/Projects/roy-web/src/lib/ConnectionsView.svelte`

- [ ] **Step 1: Replace the file**

Replace `src/lib/ConnectionsView.svelte` with:

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { Plug, RefreshCw, Search, Trash2, Plus } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { connectionsStore } from './connections.svelte';
  import { providersStore } from './providers.svelte';
  import ConnectDialog from './ConnectDialog.svelte';
  import ProviderIcon from './ProviderIcon.svelte';
  import type { Provider } from './providers.svelte';
  import type { Connection } from './connections.svelte';

  let query = $state('');
  let selectedId = $state<string | null>(null);
  let dialogOpen = $state(false);

  onMount(() => {
    void connectionsStore.load();
    void providersStore.load();
  });

  // Group user's connections by provider_id so the left pane can render
  // "Connected" entries one-per-provider, each expandable to instances.
  const grouped = $derived.by(() => {
    const map = new Map<string, Connection[]>();
    for (const c of connectionsStore.list) {
      if (!c.provider_id) continue;
      const arr = map.get(c.provider_id) ?? [];
      arr.push(c);
      map.set(c.provider_id, arr);
    }
    return map;
  });

  const connectedProviders = $derived(
    providersStore.list.filter((p) => grouped.has(p.id)),
  );
  const availableProviders = $derived(
    providersStore.list.filter((p) => !grouped.has(p.id)),
  );

  const filteredConnected = $derived(filterByQuery(connectedProviders, query));
  const filteredAvailable = $derived(filterByQuery(availableProviders, query));

  function filterByQuery(arr: Provider[], q: string): Provider[] {
    const norm = q.trim().toLowerCase();
    if (!norm) return arr;
    return arr.filter(
      (p) =>
        p.name.toLowerCase().includes(norm) ||
        p.description.toLowerCase().includes(norm),
    );
  }

  const selected = $derived(
    selectedId ? providersStore.list.find((p) => p.id === selectedId) ?? null : null,
  );

  async function disconnect(c: Connection) {
    try {
      await connectionsStore.remove(c.id);
    } catch (e) {
      console.error('disconnect failed', e);
    }
  }
</script>

<div class="flex h-full min-h-0 w-full">
  <!-- Left pane: list -->
  <aside class="w-72 shrink-0 border-r border-border/40 bg-background/95 flex flex-col">
    <header class="border-b border-border/40 px-4 py-3">
      <div class="flex items-center justify-between gap-2 mb-3">
        <h1 class="flex items-center gap-2 text-sm font-semibold">
          <Plug class="size-4 text-muted-foreground" /> Connections
        </h1>
        <Button
          variant="ghost"
          size="icon"
          onclick={() => {
            void connectionsStore.load(true);
            void providersStore.load(true);
          }}
          aria-label="Refresh"
        >
          <RefreshCw class={['size-3.5', (connectionsStore.loading || providersStore.loading) ? 'animate-spin' : '']} />
        </Button>
      </div>
      <div class="relative">
        <Search class="absolute left-2 top-1/2 size-3 -translate-y-1/2 text-muted-foreground" />
        <Input
          bind:value={query}
          placeholder="Search"
          class="h-8 pl-7 text-sm"
          autocomplete="off"
        />
      </div>
    </header>

    <div class="flex-1 overflow-y-auto p-2 space-y-4">
      {#if filteredConnected.length > 0}
        <section>
          <h2 class="px-2 mb-1 text-[10px] uppercase tracking-wider text-muted-foreground">
            Connected
          </h2>
          {#each filteredConnected as p (p.id)}
            <button
              type="button"
              onclick={() => (selectedId = p.id)}
              class={[
                'w-full flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40',
                selectedId === p.id ? 'bg-accent/60' : '',
              ]}
            >
              <ProviderIcon name={p.icon} class="size-4" />
              <span class="truncate flex-1">{p.name}</span>
              <span class="text-[10px] text-muted-foreground">
                {grouped.get(p.id)!.length}
              </span>
            </button>
          {/each}
        </section>
      {/if}

      {#if filteredAvailable.length > 0}
        <section>
          <h2 class="px-2 mb-1 text-[10px] uppercase tracking-wider text-muted-foreground">
            Available
          </h2>
          {#each filteredAvailable as p (p.id)}
            <button
              type="button"
              onclick={() => (selectedId = p.id)}
              class={[
                'w-full flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-muted-foreground hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40',
                selectedId === p.id ? 'bg-accent/60' : '',
              ]}
            >
              <ProviderIcon name={p.icon} class="size-4" />
              <span class="truncate flex-1">{p.name}</span>
            </button>
          {/each}
        </section>
      {/if}

      {#if providersStore.list.length === 0 && !providersStore.loading}
        <p class="px-2 text-xs text-muted-foreground">
          Catalog is empty. Edit <code class="rounded bg-muted px-1 font-mono">~/.roy/connections.yaml</code>
          to add providers.
        </p>
      {/if}
    </div>
  </aside>

  <!-- Right pane: details -->
  <main class="flex-1 overflow-y-auto">
    {#if selected}
      {@const instances = grouped.get(selected.id) ?? []}
      <div class="max-w-2xl px-8 py-6 space-y-6">
        <header class="flex items-start gap-4">
          <ProviderIcon name={selected.icon} class="size-10" />
          <div class="flex-1 min-w-0">
            <h2 class="text-xl font-semibold">{selected.name}</h2>
            <p class="text-sm text-muted-foreground mt-1">{selected.description}</p>
          </div>
          <Button onclick={() => (dialogOpen = true)}>
            <Plus class="size-4" />
            {instances.length === 0 ? 'Connect' : 'Connect another'}
          </Button>
        </header>

        {#if instances.length > 0}
          <section>
            <h3 class="text-sm font-medium mb-2">Connected instances</h3>
            <div class="space-y-2">
              {#each instances as c (c.id)}
                <div class="flex items-center gap-3 rounded-md border border-border/40 px-4 py-2.5">
                  <div class="flex-1 min-w-0">
                    <p class="text-sm font-mono">{c.name}</p>
                    <p class="text-[11px] text-muted-foreground">
                      Created {new Date(c.created_at * 1000).toLocaleDateString()}
                    </p>
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    onclick={() => void disconnect(c)}
                    aria-label="Disconnect"
                    class="text-destructive hover:bg-destructive/10"
                  >
                    <Trash2 class="size-4" />
                  </Button>
                </div>
              {/each}
            </div>
          </section>
        {/if}
      </div>

      <ConnectDialog provider={selected} bind:open={dialogOpen} />
    {:else}
      <div class="h-full flex items-center justify-center text-sm text-muted-foreground">
        Select a provider from the list.
      </div>
    {/if}
  </main>
</div>
```

- [ ] **Step 2: Make sure `ProviderIcon` accepts string `name`**

Look at `src/lib/ProviderIcon.svelte`. It currently routes off `name` for ACP presets. Add a fallback case for catalog icons:

If `name` is "github" → render the GitHub Lucide icon (`Github` from `@lucide/svelte`). For everything else (no other catalog icon in MVP), fall back to a generic `Plug` icon.

Add to `src/lib/provider-icons.ts`:

```ts
// Extension: catalog provider icon resolution. Returns the Lucide component
// name to render; the actual lookup table lives in ProviderIcon.svelte.
export function providerIcon(catalogIcon: string): string {
  if (catalogIcon === 'github') return 'Github';
  return 'Plug';
}
```

Wire `ProviderIcon.svelte` to fall back to `providerIcon(name)` when `name` isn't an AgentPreset.

- [ ] **Step 3: Check**

Run: `npm run check 2>&1 | tail -10`
Expected: 0 errors.

- [ ] **Step 4: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-web add \
  src/lib/ConnectionsView.svelte \
  src/lib/ProviderIcon.svelte \
  src/lib/provider-icons.ts
git -C /Users/i_strelov/Projects/roy-web commit -m "feat(web): two-pane Connections page with catalog support"
```

### Task B5: ConnectionPicker label tweak

**Files:**
- Modify: `/Users/i_strelov/Projects/roy-web/src/lib/ConnectionPicker.svelte`

- [ ] **Step 1: Render `provider.name · connection.name` when provider_id is set**

Find where each connection row is rendered (`{#each connectionsStore.list as c}`). Wrap the display in a derived label:

```svelte
<script lang="ts">
  // ... existing imports ...
  import { providersStore } from './providers.svelte';

  // ... existing state ...

  onMount(() => {
    void connectionsStore.load();
    void providersStore.load();
  });

  function displayLabel(c: import('./connections.svelte').Connection): string {
    if (c.provider_id) {
      const p = providersStore.get(c.provider_id);
      if (p) return `${p.name} · ${c.name}`;
    }
    return c.name;
  }
</script>

<!-- in the row template: -->
<span class="truncate">{displayLabel(c)}</span>
```

The trigger button counter ("Connections: N selected") doesn't need changes.

- [ ] **Step 2: Check + build**

Run: `npm run check && npm run build 2>&1 | tail -10`
Expected: 0 errors. Bundle build successful.

- [ ] **Step 3: Commit**

```bash
git -C /Users/i_strelov/Projects/roy-web add src/lib/ConnectionPicker.svelte
git -C /Users/i_strelov/Projects/roy-web commit -m "feat(web): ConnectionPicker shows provider name when catalog-backed"
```

### Task B6: End-to-end visual check (manual)

Not a test commit — a verification gate before opening the PR. Confirms the whole flow works against the running infrastructure (daemon + management + gateway + roy-web dev).

- [ ] **Step 1: Make sure the backend has shipped Phase A**

Either rebase the frontend branch on top of a backend image where Phase A is merged, or run a local management built from `feat/connection-catalog` of the backend repo.

- [ ] **Step 2: Place a real catalog**

```bash
cp /Users/i_strelov/Projects/roy-connections-mcp/crates/roy-management/resources/connections.default.yaml \
   ~/.roy/connections.yaml
```

Restart management. Confirm `tracing` reports `providers = 1`.

- [ ] **Step 3: Open `/connections` in the browser**

Should see "GitHub" under **Available**. Click → right pane shows GitHub details + "Connect" button.

- [ ] **Step 4: Click Connect**

Fill `Label = work`, paste a GitHub PAT. Submit. Dialog closes; left pane now shows GitHub under **Connected** with count `1`.

- [ ] **Step 5: Click "Connect another"**

Label = `personal`, paste another PAT. Should work. Two instances shown.

- [ ] **Step 6: Try duplicate**

Click "Connect another", set Label = `work` again. Expect inline error: "a connection with this provider and label already exists". Dialog stays open.

- [ ] **Step 7: Open new chat with claude preset**

Composer's ConnectionPicker should now list both "GitHub · work" and "GitHub · personal" as selectable.

- [ ] **Step 8: Disconnect**

In `/connections`, click the trash icon on one instance. Confirm it disappears.

If any step fails, **stop** and report the failure with the relevant log tail (management log + browser network tab).

---

## Self-review

**1. Spec coverage:**
- YAML schema (`Provider`, `SecretSchema`, default file) → Tasks A3, A4 (default content + tests).
- Boot policy (missing/empty/broken) → Task A5 + A4 tests.
- DB schema delta (column + index) → Task A2.
- `GET /providers` → Task A8.
- `POST /connections` catalog flow + 409 + missing-secret + unknown-id → Tasks A7, A9.
- `Connection.provider_id` on `GET /connections` → Task A6.
- Two-pane UI + per-provider Connect dialog → Tasks B3, B4.
- Composer picker label update → Task B5.
- Manual end-to-end → Task B6.

**2. Placeholder scan:** No "TBD"/"add validation"/"similar to". The one place that says "rest is a copy of `test_app`" (A9 Step 1) describes a test-harness duplication that's explicitly small + bounded; the implementer can `grep test_app` and clone. If the reviewer flags it as too vague, expand it inline at execution time.

**3. Type consistency:** `Provider`, `Connection.provider_id`, `NewConnectionFromProvider` use identical field names on backend and frontend (`provider_id`, `name`, `secrets`, `command`, `args`, `env`). `Catalog::get` and `providersStore.get` mirror each other. `Catalog::from_providers` is the only test-only constructor; gated behind `test-support`.

**4. Out of MVP, called out:**
- "+ Add custom MCP server" UI button — not in this plan, current free-form form goes away from the UI but `NewConnectionCustom` POST stays valid for tests/CLI.
- Icon set beyond GitHub — added per provider as needed.
- HTTP/SSE upstream MCPs, OAuth, secrets-at-rest encryption — original plan's follow-ups, unchanged.
