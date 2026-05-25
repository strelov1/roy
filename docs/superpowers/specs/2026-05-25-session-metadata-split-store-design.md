# Session Metadata Split-Store

**Status:** design accepted, ready for plan
**Date:** 2026-05-25

## Problem

`roy` core хранит per-session-метаданные в `<journal_dir>/<session_id>.meta.json`, а реестр проектов — в `<journal_dir>/projects.json`. Это плохо по трём причинам:

1. **Core знает о проектах.** `SessionManager` держит `Arc<ProjectRegistry>`, дёргает `ensure_project` / `register_session` при spawn и resume. Это смешивает process-management (что core'у действительно нужно делать) с каталогизацией (что является UX-задачей).
2. **Файловый формат для расширяемой меты.** Добавить новое поле (tags, display label, агентский custom-state) требует версионирования JSON-структуры на каждой сессии и линейного скана dir'а для list/filter. SQLite уже принятый шаблон в проекте (`roy-agents`, `roy-scheduler`).
3. **Связка через `*.meta.json`+`projects.json` не атомарна** и race'ится с in-flight turn'ом (`SetTags` идёт через actor's mpsc + persist_metadata после каждого turn'а).

## Goals

- Вытащить projects из core полностью. Core больше не знает термин «project».
- Дать богатой мете (tags, display-label, agent_name, кастом-поля) индексируемое хранилище в SQLite.
- Сохранить автономность daemon'а: он стартует и `resume_all`-ит без запущенного `roy-management`.

## Non-goals

- Миграция существующих `.meta.json` / `projects.json` — пользователей нет, проект в активной разработке. Upgrade-шаг: `rm -rf ~/.roy/journals && rm -f ~/.roy/projects.json`.
- Replacing `*.jsonl` журналы — append-only event log остаётся файловым.
- Multi-daemon / multi-machine. Один daemon на хост.

## Architecture

Split-store с координатором на стороне `roy-management`:

- **`roy` core** владеет `~/.local/state/roy/sessions.db` (boot-kit, путь override через `ROY_SESSIONS_DB`). Одна таблица `sessions` с минимумом, необходимым для resume агентского процесса.
- **`roy-management`** добавляет три таблицы (`projects`, `session_meta`, `session_tags`) в существующую `~/.local/state/roy/agents.db` через миграции. `roy-agents` остаётся owner'ом своего pool'а и таблицы `agents`; management навешивает свои таблицы поверх.
- Связь между двумя БД — по `session_id` (UUID). Без cross-DB FK; rollback — через compensating actions (Close), не distributed tx.
- Daemon **независим** от management. `roy serve` стартует и работает без `roy management run`. Project-/tag-aware операции CLI идут через HTTP-API management.

```
┌────────────────────┐         HTTP        ┌──────────────────────┐
│      roy-cli       │ ──────────────────▶ │    roy-management    │
│                    │                     │                      │
│ project/tag cmds   │                     │  POST /sessions      │
│ list --rich        │                     │  POST /projects      │
│                    │     Unix socket     │  PUT  /sessions/.../ │
│ orphan spawn,      │ ──────────────────▶ │       tags           │
│ attach, list, wait │                     │  GET  /sessions      │
└────────────────────┘                     └──────────┬───────────┘
                              ▲                       │
                              │       Unix socket     │
                              │ ClientCommand::Spawn  │
                              │   {agent, cwd, ...}   │
                              │                       ▼
                          ┌───┴──────────────────────────────┐
                          │            roy daemon            │
                          │   sessions.db   journals/*.jsonl │
                          └──────────────────────────────────┘
```

## Data Model

### Core SQLite — `~/.local/state/roy/sessions.db`

```sql
CREATE TABLE sessions (
  session_id     TEXT PRIMARY KEY,
  agent          TEXT NOT NULL,           -- preset: claude/gemini/opencode/codex
  cwd            TEXT NOT NULL,
  model          TEXT,
  permission     TEXT,
  resume_cursor  TEXT,                    -- mutates per-turn
  system_prompt  TEXT,                    -- persona snapshot at spawn
  created_at     INTEGER NOT NULL,        -- unix seconds
  closed_at      INTEGER                  -- NULL = live, set on Close
);
CREATE INDEX sessions_live ON sessions(closed_at) WHERE closed_at IS NULL;
```

Boot-kit-only. **Никаких** `project_id`, `tags`, `agent_name` в core.

### Management SQLite — добавления в `~/.local/state/roy/agents.db`

```sql
CREATE TABLE projects (
  id         TEXT PRIMARY KEY,
  name       TEXT UNIQUE NOT NULL,        -- [A-Za-z0-9_-]+, no '.' prefix
  path       TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE session_meta (
  session_id    TEXT PRIMARY KEY,         -- echoed from core (no cross-DB FK)
  project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
  agent_id      TEXT REFERENCES agents(id) ON DELETE SET NULL,
  agent_name    TEXT,                     -- display label (snapshot at spawn)
  display_label TEXT,                     -- user-renamable
  created_at    INTEGER NOT NULL
);
CREATE INDEX session_meta_project ON session_meta(project_id);

CREATE TABLE session_tags (
  session_id TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL,
  PRIMARY KEY (session_id, key)
);
CREATE INDEX session_tags_key_value ON session_tags(key, value);
```

`ON DELETE SET NULL` на `project_id` означает: удалили проект — `session_meta` rows остаются, отвязываются. Журналы и core-rows не трогаются. Каскад — только в очевидно-удалительных endpoints.

## Components

### crates/roy (core) — изменения

- **Новый модуль `session_store.rs`** — sqlx pool + CRUD над `sessions`. API:

  ```rust
  pub struct SessionStore { pool: SqlitePool }

  impl SessionStore {
      pub async fn open(path: &Path) -> Result<Self>;
      pub async fn insert(&self, row: SessionRow) -> Result<()>;
      pub async fn update_cursor(&self, sid: &str, cursor: Option<&str>) -> Result<()>;
      pub async fn update_model(&self, sid: &str, model: Option<&str>) -> Result<()>;
      pub async fn mark_closed(&self, sid: &str) -> Result<()>;
      pub async fn delete(&self, sid: &str) -> Result<()>;
      pub async fn get(&self, sid: &str) -> Result<Option<SessionRow>>;
      pub async fn list_live(&self) -> Result<Vec<SessionRow>>;
      pub async fn list_archived(&self) -> Result<Vec<SessionRow>>;
  }
  ```

- **Удаляются**:
  - `crates/roy/src/session_meta.rs` (файл целиком)
  - `crates/roy/src/project.rs` (файл целиком)
  - Из `SessionManager`: `Arc<ProjectRegistry>`, `register_session`, `unregister_session`, `ensure_project`, `allocate_orphan_session_dir`, `index_existing_sessions`. Orphan-cwd-allocation мигрирует в простую функцию внутри `daemon::handle_spawn`.
  - Из `SessionEngine`: поля `project_id` и `tags` + методы `set_tags`, `tags()`.

- `SessionManager::list_archived` больше не сканирует filesystem — читает `SessionStore::list_archived()`.
- `resume_all` идёт по той же таблице.
- `delete_archive` удаляет (journal-file, sessions-row).

### crates/roy-management — изменения

- **Новый модуль `meta_store.rs`** — CRUD над `projects`, `session_meta`, `session_tags`. Шарит pool с `roy_agents::Store`.
- **Новый trait `DaemonClient`** (см. Testing) для замены прямого Unix-socket доступа на mock-able абстракцию.
- **Новые HTTP endpoints** в `http.rs`:

  | Method | Path | Назначение |
  |--------|------|-----------|
  | `GET /projects` | список проектов | |
  | `POST /projects` | `{name}` → новый проект | |
  | `DELETE /projects/{id}` | `ON DELETE SET NULL` для session_meta | |
  | `POST /sessions` | координирует spawn (см. Data Flow) | |
  | `GET /sessions` | join core (`List`) + session_meta + session_tags | |
  | `GET /sessions/{id}` | детали одной сессии | |
  | `PUT /sessions/{id}/tags` | заменить теги | |
  | `PATCH /sessions/{id}` | переименовать display_label, agent_name | |

- `POST /agents/{id}/run` становится тонкой обёрткой над `POST /sessions`.

### crates/roy-cli — изменения

CLI получает HTTP-клиент к management:

| Команда | Адресат |
|---------|---------|
| `roy projects {list,create,delete}` | management |
| `roy run --project foo` | management `POST /sessions` |
| `roy run --cwd /tmp` (orphan) | daemon |
| `roy set-tags` | management `PUT /sessions/{id}/tags` |
| `roy list` | daemon (id+state из core) |
| `roy list --rich` | management |
| `roy attach`, `wait`, `close`, `status`, `resume` | daemon |

Management URL — новый env `ROY_MANAGEMENT_URL` (default `http://127.0.0.1:8079`, под `ROY_MANAGEMENT_ADDR` который сейчас уже использует management). При отсутствии запущенного management — project/tag-команды fail-ят с понятной ошибкой.

## Wire Protocol Delta

**Removed:**
- `ClientCommand::SetTags`
- `ClientCommand::ListProjects`, `CreateProject`, `DeleteProject`
- `ServerEvent::ProjectsList`, `ProjectCreated`, `ProjectDeleted`, `SessionUpdated`
- `ErrorCode::CreateProjectFailed`, `DeleteProjectFailed`, `InvalidProjectName`, `ProjectExists`

**Changed:**
- `Spawn { project_id }` → `Spawn { cwd: Option<PathBuf> }`. `cwd: None` → daemon аллоцирует `<workspace>/<session_id>/`.
- `Spawned`, `Spawning`, `SessionRecord` теряют поле `project_id`.
- `Resume { tags }` теряет поле `tags`.

## Data Flow

### Spawn (project-aware, через management)

1. CLI → `POST /sessions {agent, project_id?, tags, agent_name, model?, permission?}`.
2. Management резолвит `project_id → cwd` (SELECT path FROM projects).
3. Management шлёт даemon-у `ClientCommand::Spawn{agent, cwd, model, permission, system_prompt}`.
4. Daemon в одной sqlx-tx делает `SessionStore::insert` + `SessionEngine::spawn` (transport open + journal). Возвращает `Spawned{session, agent, cwd}`.
5. Management в одной sqlx-tx INSERT'ит `session_meta` + N `session_tags`. Возвращает 201 с полным JSON'ом.

### Orphan spawn (CLI напрямую в daemon)

`roy run --cwd /tmp` или `roy run` без проекта:

- CLI шлёт `Spawn{cwd: None|Some(/tmp), agent, ...}` даemon-у напрямую.
- Daemon аллоцирует workspace dir если нужно, делает `SessionStore::insert` + `SessionEngine::spawn`.
- Management не вовлечён. Сессия видна в `GET /sessions` как row без `session_meta` (рендерится «без проекта»).

### Resume (после рестарта daemon'а)

- `SessionStore::list_live()` → boot-kit rows.
- Для каждого: `SessionManager::resume(row)` → factory + engine.
- Management не участвует. Если он стартует позже — его мета остаётся согласованной, session_id'ы те же.

### SetTags (только management)

- CLI → `PUT /sessions/{sid}/tags`.
- Management в одной tx: `DELETE FROM session_tags WHERE sid=? ; INSERT ...`.
- Daemon не вовлечён → нет race с in-flight turn'ом.

### List sessions (rich)

- CLI → `GET /sessions`.
- Management: запрашивает у daemon `List` + `ListArchived`, делает SELECT с join'ом `session_meta`/`session_tags`, мерджит outer-join'ом. Сессии без меты → «без проекта».
- Без кеша — каждый раз спрашиваем daemon. Десктоп-нагрузка не оправдывает кеш + invalidation.

## Error Handling

### Spawn rollback

| Failure point | Поведение |
|---------------|-----------|
| Management не зарезолвил project_id | `400 invalid_project`; daemon не вызывается |
| Core `SessionStore::insert` упал | tx откатывается; daemon → `Error`; management → `502 spawn_failed` |
| `SessionEngine::spawn` упал | tx с insert откатывается атомарно; management → `502` |
| Management `INSERT session_meta` упал после успешного `Spawned` | management шлёт `Close{session}` best-effort; CLI получает `500 meta_persist_failed; session was created and closed: <sid>` |
| `Close` тоже упал | warn-лог; CLI получает `500 meta_persist_failed; session may be live: <sid>`; orphan-sweep (см. ниже) подберёт |

### Resume

- `SessionStore::list_live()` падает (corrupted .db) → fatal, daemon отказывается стартовать. Не пытаемся auto-recover.
- Per-session resume падает (preset бинарник пропал) → `(sid, Some(err))`, row остаётся; ручное `delete_archive` или `Resume` после починки.
- Management row отсутствует для core-сессии → **не ошибка**: orphan-spawn выглядит так.

### Cleanup orphan management rows

Management background task раз в 10 минут (off через `ROY_MGMT_ORPHAN_SWEEP=off`):

1. Запросить `daemon::List` + `daemon::ListArchived`.
2. `DELETE FROM session_meta WHERE session_id NOT IN (...)`.

Обратное (orphan core → INSERT в management) **не делаем** — orphan-сессии без меты разрешены семантикой.

### Concurrency / locks

- Core `sessions.db` — single-writer (`PidLock` на daemon-сокете уже это гарантирует).
- Management `agents.db` — sqlx pool + WAL. Добавляем `PidLock` для management для симметрии.
- Cross-DB операций нет; rollback — через compensating Close.

## Testing

### Core (crates/roy)

`session_store.rs` unit-тесты (sqlx `:memory:`):
- insert + get roundtrip
- update_cursor / update_model / mark_closed
- list_live vs list_archived фильтрация
- delete idempotent

`SessionManager` integration-тесты (адаптируем существующие):
- `spawn_then_list_live`
- `close_marks_archived`
- `resume_all_brings_back_closed_sessions` (переписать под SessionStore)
- `resume_skips_dead_preset` (preset бинарник недоступен)
- `delete_archive_removes_row_and_journal`
- Удаляются все тесты `ProjectRegistry` и `index_existing_sessions`.

### Management (crates/roy-management)

`meta_store.rs` unit-тесты (sqlx `:memory:`):
- create_project + list
- `DELETE projects/{id}` устанавливает `session_meta.project_id = NULL`
- insert session_meta + tags + replace_tags

HTTP handler-тесты (axum `oneshot` + `MockDaemonClient`):
- `POST /projects` happy + 409 на дубликат
- `POST /sessions` happy → 201
- `POST /sessions` rollback path (meta insert fails → Close emitted → 500)
- `PUT /sessions/{id}/tags`
- `DELETE /projects/{id}` оставляет session_meta rows с NULL project_id

### `DaemonClient` trait для mocking

```rust
#[async_trait]
pub trait DaemonClient: Send + Sync {
    async fn spawn(&self, req: SpawnRequest) -> Result<String>;
    async fn close(&self, session_id: &str) -> Result<()>;
    async fn list(&self) -> Result<Vec<String>>;
}
```

`AppState` держит `Arc<dyn DaemonClient>`. Production-импл — `UnixSocketDaemonClient`. Test-импл — `MockDaemonClient` с настраиваемым поведением.

### Wire-protocol contract tests

`control.rs` serde-roundtrip тесты обновить:
- Убрать тесты для удалённых вариантов (`SetTags`, `ListProjects`, `CreateProject`, `DeleteProject`, `ProjectsList`, `SessionUpdated`, `ProjectCreated`, `ProjectDeleted`).
- Поправить `Spawn` (`project_id` → `cwd`).
- Поправить `Spawned`/`Spawning`/`SessionRecord` (удалить `project_id`).

### E2E smoke (один тест, `#[ignore]`)

`crates/roy-management/tests/e2e_spawn.rs`:
- Запускает реальный `roy serve` (tmp socket) + `roy management run` (на `:0` порт).
- `POST /projects` → `POST /sessions` с этим project.
- Проверяет 201 и что сессия видна в обоих: `roy list` и `GET /sessions`.

### CI

Без изменений — `cargo fmt --all -- --check`, `cargo build --workspace --all-targets`, `cargo test --workspace --no-fail-fast`. `python3` для fake-ACP остаётся.

## Upgrade Notes

Breaking change. CHANGELOG/README документирует:

```bash
# Before upgrading:
rm -rf ~/.roy/journals      # old session journals + .meta.json
rm -f  ~/.roy/projects.json
```

Новые БД создаются автоматически при первом старте daemon (`sessions.db`) и management (`projects` + `session_meta` + `session_tags` миграции).
