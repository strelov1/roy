# User entity, authorization, per-user cwd, commands discovery

**Дата:** 2026-05-25
**Статус:** design — готов к writing-plans
**Скоуп:** первая итерация мульти-пользовательской модели в roy

## Проблема

Сейчас roy — single-tenant: один общий WS-токен в файле, никакой `users`-таблицы, у `projects`/`session_meta`/`agents` нет колонки владельца, у сессии `cwd` приходит произвольным от клиента. Чтобы поднять roy в режиме «несколько людей в одном инстансе», нужны три связанных кирпича:

1. **User entity + auth.** Username/password → JWT cookie, один и тот же JWT едет в HTTP cookie и в `Sec-WebSocket-Protocol`.
2. **Per-scope cwd.** `cwd` сессии живёт в дереве, привязанном к пользователю (или к команде, если scope=team). Это даёт ACP-агенту контекст пользователя через walk-up к `CLAUDE.md`, который пишет сам пользователь.
3. **Commands discovery.** Slash-команды (`/<skill-name>`) обнаруживаются скан-проходом по `~/.claude/skills/**` и плагин-marketplace'ам, как делает `bridge/skills.ts` в claude-agent. БД не используется.

Reference-реализация: `/Users/i_strelov/Projects/claude-agent` (Next.js + bridge на Node). В roy переносим **идеи**, не код: stack другой (Rust workspace), HTTP-фасад другой (axum в `roy-management`), и daemon остаётся trusted.

## Архитектура

### Раскладка крейтов

Добавляется новый library-крейт `roy-auth`. Зависимости:

```
roy-management ─┬─▶ roy-auth ─┬─▶ shared sqlite pool (agents.db)
                │             │
roy-gateway   ──┘             │
                              │
roy (daemon)                  │   ← НЕ зависит от roy-auth, остаётся trusted
                              │
roy-cli       ─▶ HTTP client  │
                              │
                              ▼
                       JWT helpers + user/team store
```

`roy-auth` владеет:
- Таблицами `users`, `teams`, `team_members`, `team_invites` (миграции 0010+).
- Типами `User`, `Team`, `TeamMembership`, `TeamInvite`, `UserProfile`.
- JWT-хелперами (`sign_session`, `verify_session`, `verify_cookie`, `verify_ws_protocol`).
- bcrypt-обёрткой (`hash_password`, `verify_password`).
- ACL-хелперами (`Acl::can_access_scope`, `can_admin_team`, `can_access_project`).
- `pub mod test_support` под cfg-флагом `test-support` (temp-pool, make_user, make_team, issue_jwt).

Никаких HTTP-handler'ов внутри `roy-auth` — только store + хелперы.

### Trust boundary

```
untrusted: browser, Telegram, network roy-cli
                │
                ▼
═════════ AUTH BOUNDARY ═════════
roy-management (HTTP)            ← JWT verify в middleware
roy-gateway   (WS handshake)     ← Sec-WebSocket-Protocol verify
                │
                ▼ Unix socket, mode 0600
═════════ TRUSTED ZONE ══════════
roy daemon                       ← никакого JWT,
                                   ClientCommand::Spawn { cwd, ... }
                                   с уже резолвленным cwd
```

Все ACL-проверки случаются **до** `ClientCommand::Spawn`. Прямой доступ к Unix-сокету означает trusted (root-эквивалент на машине) — это стандартный Unix-подход и упрощает daemon.

`roy-cli` для локальных команд (`run`, `attach`, `list`) обходится без JWT через Unix-сокет.

## Модель данных

Существующих рядов нет — миграции не делают переходных шагов, сразу финальная форма с `NOT NULL` на колонках владения.

### Новые таблицы (`roy-auth`, миграции 0010–0012)

```sql
-- 0010_users.sql
CREATE TABLE users (
    id            TEXT PRIMARY KEY,                    -- uuid v4
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    password_hash TEXT NOT NULL,                       -- bcrypt
    timezone      TEXT,                                -- IANA, nullable
    created_at    INTEGER NOT NULL                     -- unix ms
);

-- 0011_teams.sql
CREATE TABLE teams (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    created_by  TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE team_members (
    user_id   TEXT NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
    team_id   TEXT NOT NULL REFERENCES teams(id)  ON DELETE CASCADE,
    role      TEXT NOT NULL DEFAULT 'member',         -- 'owner' | 'member'
    joined_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, team_id)
);

CREATE INDEX team_members_by_team ON team_members(team_id);

-- 0012_team_invites.sql
CREATE TABLE team_invites (
    token        TEXT PRIMARY KEY,                    -- 32 hex chars
    team_id      TEXT NOT NULL REFERENCES teams(id)  ON DELETE CASCADE,
    created_by   TEXT NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    accepted_by  TEXT REFERENCES users(id)  ON DELETE SET NULL,
    accepted_at  INTEGER
);
```

### Изменения существующих таблиц (`roy-management`, миграция 0005)

```sql
-- 0005_owners.sql
DELETE FROM session_tags;
DELETE FROM session_meta;
DELETE FROM projects;

DROP TABLE projects;
CREATE TABLE projects (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    path       TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users(id),
    team_id    TEXT REFERENCES teams(id),            -- NULL = personal
    created_at INTEGER NOT NULL
);

DROP TABLE session_meta;
CREATE TABLE session_meta (
    session_id    TEXT PRIMARY KEY,
    project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
    agent_id      TEXT,
    agent_name    TEXT,
    display_label TEXT,
    created_by    TEXT NOT NULL REFERENCES users(id),
    team_id       TEXT REFERENCES teams(id),         -- NULL = personal
    created_at    INTEGER NOT NULL
);
-- session_tags пересоздаётся пустой (FK на session_meta остаётся валидным).
```

`agents` (из `roy-agents`) остаётся глобальной до отдельного будущего Plan C — это согласовано с CLAUDE.md.

### Координация версий миграций

```
roy-agents      → 0001-0003
roy-management  → 0004-0009 (резерв)
roy-auth        → 0010-0019 (резерв)
```

Каждый крейт держит свой `Migrator` с `set_ignore_missing(true)`. Порядок запуска при старте `roy-management`: `roy_agents::open()` → `roy_auth::apply_migrations()` → `MetaStore::apply_migrations()`.

### Bootstrap-root (startup-код, не SQL)

```rust
if !roy_auth::has_any_user(&pool).await? {
    let pw = env::var("ROY_BOOTSTRAP_PASSWORD")
        .unwrap_or_else(|_| {
            let hex = random_hex(32);
            eprintln!("roy: bootstrap user 'root' — password: {hex}");
            hex
        });
    roy_auth::create_user(&pool, NewUser {
        username:     env::var("ROY_BOOTSTRAP_USERNAME").unwrap_or("root".into()),
        display_name: env::var("USER").unwrap_or("root".into()),
        password:     pw,
    }).await?;
}
```

## Per-scope cwd (workspace layout)

```
$ROY_WORKSPACE_DIR  (default ~/.roy/workspace)
├── users/<user_id>/
│   ├── sessions/<session_id>/                 ← cwd для personal session
│   └── projects/<project_id>/sessions/<session_id>/
└── teams/<team_id>/
    ├── sessions/<session_id>/
    └── projects/<project_id>/sessions/<session_id>/
```

**roy создаёт только `cwd`-директорию через `mkdir -p`. Никакой автогенерации `CLAUDE.md`, `.memory/` и любых других файлов.** Если пользователь хочет, чтобы агент видел контекст «кто я / в какой команде / какой проект» — он сам кладёт `CLAUDE.md` в соответствующий каталог `users/<id>/`, `teams/<id>/` или `projects/<id>/`. ACP-агент сам найдёт его walk-up'ом.

Это сознательный выбор: zero-magic, единое поведение для всех преcет'ов (claude/gemini/opencode/codex), никаких per-preset бранчей в roy-management.

### Резолвинг `cwd` на стороне `roy-management`

```rust
fn resolve_cwd(scope: Scope, user: &str, team: Option<&str>,
               project: Option<&str>, session: &str) -> Result<PathBuf> {
    let root = match scope {
        Scope::Personal => workspace_dir().join("users").join(user),
        Scope::Team     => workspace_dir().join("teams").join(team.unwrap()),
    };
    let path = match project {
        Some(p) => root.join("projects").join(p).join("sessions").join(session),
        None    => root.join("sessions").join(session),
    };
    require_safe_path(&path)?;          // защита от path-traversal
    Ok(path)
}

fn require_safe_path(p: &Path) -> Result<()> {
    let workspace = workspace_dir().canonicalize()?;
    let parent    = p.parent().ok_or(invalid())?;
    create_dir_all(parent)?;
    let canonical = parent.canonicalize()?;
    if !canonical.starts_with(&workspace) {
        return Err(Forbidden);
    }
    Ok(())
}
```

Дополнительно — UUID-shape check (`^[a-f0-9-]{36}$`) на `user_id`/`team_id`/`project_id`/`session_id`.

## HTTP API

Все эндпоинты на `roy-management`. Middleware `require_user` применяется ко всему роутеру, **кроме** `/auth/login` и `/auth/accept-invite`.

| Метод | Путь | Body | Ответ | ACL |
|---|---|---|---|---|
| `POST` | `/auth/login` | `{username, password}` | `{user: UserProfile}` + `Set-Cookie` | — |
| `POST` | `/auth/logout` | — | `204` + `Set-Cookie: roy-jwt=; Max-Age=0` | logged-in |
| `GET`  | `/auth/me` | — | `UserProfile` | logged-in |
| `POST` | `/auth/invites` | `{teamId, expiresAt?}` | `{token, url}` | team owner |
| `POST` | `/auth/accept-invite` | `{token, username?, password?}` | `{user, team}` | — |
| `GET`  | `/teams` | — | `TeamInfo[]` | logged-in (только свои) |
| `POST` | `/teams` | `{name, description?}` | `TeamInfo` | logged-in |
| `DELETE` | `/teams/{id}` | — | `204` | team owner |
| `GET`  | `/commands` | — | `CommandInfo[]` | logged-in |
| `POST` | `/sessions` | `{scope, teamId?, projectId?, preset, agent?, ...}` | `SessionMeta` | scope ACL |
| `POST` | `/projects` | `{name, scope, teamId?}` | `Project` | scope ACL |

### Типы (re-export в `roy-management`)

```rust
pub struct UserProfile {
    pub id:           String,
    pub username:     String,
    pub display_name: String,
    pub timezone:     Option<String>,
    pub teams:        Vec<TeamMembership>,
}

pub struct TeamMembership {
    pub id:   String,
    pub name: String,
    pub role: Role,             // Owner | Member
}

pub struct CommandInfo {
    pub name:        String,    // e.g. "review"
    pub description: String,
    pub source:      String,    // "user" | plugin id
}
```

### Middleware

```rust
async fn require_user(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let cookie = req.headers().get(header::COOKIE);
    let user_id = roy_auth::verify_cookie(&state.pool, cookie).await
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "auth required".into()))?;
    req.extensions_mut().insert(AuthUser(user_id));
    Ok(next.run(req).await)
}
```

В handler'ах:

```rust
async fn create_session(
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    State(state): State<AppState>,
    Json(body): Json<CreateSessionReq>,
) -> Result<Json<SessionMeta>, ApiError> {
    let acl = roy_auth::Acl::new(&state.pool, &user_id);
    acl.can_access_scope(&body.scope).await?;
    if let Some(pid) = &body.project_id {
        acl.can_access_project(pid).await?;
    }
    let session_id = uuid_v4();
    let cwd = resolve_cwd(body.scope, &user_id, body.team_id.as_deref(),
                          body.project_id.as_deref(), &session_id)?;
    create_dir_all(&cwd)?;
    state.meta_store.insert_session(SessionMeta { id: session_id.clone(),
        created_by: user_id.clone(), team_id: body.team_id.clone(),
        project_id: body.project_id.clone(), ... }).await?;
    state.daemon.spawn(ClientCommand::Spawn { cwd, preset: body.preset, ... }).await?;
    Ok(Json(meta))
}
```

### Data flow: `POST /sessions`

```
client ─▶ POST /sessions {scope:"team", teamId:"T1", projectId:"P9", preset, ...}
         Cookie: roy-jwt=...
              │
              ▼
roy-management::create_session
  1. middleware ⇒ user_id = "U3"
  2. acl.can_access_scope(Team("T1"))           ─▶ team_members SELECT
  3. acl.can_access_project("P9")               ─▶ projects SELECT
  4. session_id = uuid_v4()
  5. cwd = resolve_cwd(...)
        = $WORKSPACE/teams/T1/projects/P9/sessions/<session_id>
  6. mkdir -p cwd
  7. INSERT session_meta (id, created_by=U3, team_id=T1, project_id=P9, ...)
  8. ClientCommand::Spawn { cwd, preset, ... } ─▶ daemon (Unix socket)
  9. response: SessionMeta JSON
```

### Commands discovery

Перенос `bridge/skills.ts` в Rust (модуль `roy-management/src/commands.rs`).

```rust
pub async fn list_commands() -> Vec<CommandInfo> {
    let mut out = scan_dir(home().join(".claude/skills"), "user").await;
    for marketplace in enabled_plugins() {
        out.extend(scan_dir(plugin_skills_dir(&marketplace), &marketplace).await);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}
```

Сканер ищет `<dir>/<name>/SKILL.md`, парсит YAML-frontmatter (handle `name`, `description`). Плагин-skills учитываются только если плагин включён в `~/.claude/settings.json` (`enabledPlugins["<plugin>@<marketplace>"]`). `enabled_plugins()` читает этот файл при каждом обновлении кеша. Кеш TTL 30 секунд.

Команды **не хранятся в БД** — это read-only view ФС. Никакого CRUD, никакого owner'а.

## WebSocket handshake (`roy-gateway`)

```
browser ─WS upgrade─▶ Sec-WebSocket-Protocol: roy-jwt,<JWT>
                                                    │
                                                    ▼
                          ws_auth_callback (переписан):
                          let token = parse_subprotocol_jwt(req)?;
                          let user_id = roy_auth::verify_ws_protocol(&pool, token).await?;
                          ctx.user_id = user_id;
                          OK 101 Switching Protocols
```

Старый файл с общим UUID-токеном (`crates/roy-gateway/src/ws.rs:49`) удаляется. Telegram-bridge (`allowed_user_ids`) остаётся как есть — отдельная ось, вне scope.

## CLI

Локальный `roy-cli` для daemon-команд (`run`, `attach`, `list`) — без JWT, через Unix-сокет. Добавляются три auth-команды:

```
roy auth login              # interactive prompt → пишет cookie в ~/.config/roy/cookie
roy auth whoami             # GET /auth/me, читает cookie из ~/.config/roy/cookie
roy auth reset <username>   # локальный override через roy_auth::set_password
                            # (recovery; ходит напрямую в БД, требует FS-доступа к agents.db)
```

`~/.config/roy/cookie` (mode 0600) используется **только** командами семейства `roy auth *` для обращения к HTTP API `roy-management`. Все остальные CLI-команды (`run`, `attach`, `list`, `wait`, `fire`, `mcp`, ...) идут напрямую в Unix-сокет daemon'а и cookie не читают — daemon остаётся trusted, как описано в Trust boundary.

`roy auth reset` обходит HTTP — это намеренно для случая «забыл пароль». Защита — file-perms 0600 на `agents.db`.

## Безопасность

### Cookie / JWT

```
roy-jwt=<JWT>; HttpOnly; SameSite=Lax; Secure (если ROY_HTTPS=1); Path=/; Max-Age=604800
```

JWT-конфиг:
- `alg: HS256`
- `secret: $ROY_JWT_SECRET` (≥32 ASCII bytes, иначе startup-fail)
- payload: `{ sub, iat, exp }`. Никакого `displayName`/`email` — резолвится из БД каждый запрос.
- TTL обновляется только через явный re-login (нет sliding renewal в MVP).

### Anti-enumeration login

`/auth/login` отвечает за константное время: при неизвестном username делается `bcrypt::verify` с dummy-хешем (вычисленным один раз при startup).

```rust
let row  = sqlx::query_as!(...).fetch_optional(&pool).await?;
let hash = row.as_ref().map(|r| r.password_hash.as_str()).unwrap_or(DUMMY_HASH);
let ok   = bcrypt::verify(&body.password, hash).unwrap_or(false);
if !ok || row.is_none() {
    return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid credentials".into()));
}
```

### Rate limit на `/auth/login`

In-memory `HashMap<IpAddr, RateBucket>` (token-bucket, 5 попыток / 5 минут, по IP). После исчерпания — `429`. Сбрасывается при рестарте процесса. Берём `X-Forwarded-For` только если задан `$ROY_TRUSTED_PROXIES`.

### Классы ошибок

| Ошибка | HTTP | Тело | Логирование |
|---|---|---|---|
| Невалидный JSON | `400` | `{"error":"invalid body"}` | warn |
| Нет cookie / битый JWT / истёкший | `401` | `{"error":"auth required"}` | debug |
| Unknown user / wrong password | `401` | `{"error":"invalid credentials"}` | warn + rate-limit counter |
| User не в team | `403` | `{"error":"forbidden"}` | warn |
| Проект не существует | `404` | `{"error":"not found"}` | debug |
| Invalid invite (consumed/expired/wrong) | `400` | `{"error":"invite invalid"}` | warn |
| FS-fail (cwd mkdir) | `500` | `{"error":"internal error"}` | error |
| Daemon недоступен | `503` | `{"error":"daemon unavailable"}` | error |
| `ROY_JWT_SECRET` не задан | exit 1 | — | fatal |
| Migration mismatch | exit 1 | — | fatal |

Принцип: наружу — generic-сообщение. Внутрь — `tracing::warn!(error=%e, user_id=?u)` с реальной причиной. Никогда не возвращаем `sqlx`-текст наружу.

### Явные YAGNI

- Password complexity rules.
- CSRF-токены (cookie уже `SameSite=Lax`, нет cross-origin форм).
- Session revocation list (jti-blacklist) — JWT живёт до `exp`.
- Audit log.
- 2FA.
- Sliding renewal TTL.
- Password reset через email — recovery через `roy auth reset` CLI.

## Тестирование

### Раскладка тестов

```
crates/roy-auth/tests/
├── jwt.rs              ─ sign/verify, expiry, wrong secret, tampered token
├── store.rs            ─ create_user/get_by_username/set_password (username collision)
└── invites.rs          ─ create/accept (consumed, expired, wrong-team)

crates/roy-management/tests/
├── auth_flow.rs        ─ login → /auth/me → logout → 401
├── acl.rs              ─ user A не видит team B; owner может удалить team
├── session_cwd.rs      ─ resolve_cwd для personal/team × project/no-project
├── path_traversal.rs   ─ `..`, `/`, не-uuid id → 403/422
├── login_constant_time.rs ─ время ответа unknown vs wrong-password (±20%)
├── rate_limit.rs       ─ 6 попыток подряд → 429 на шестой
└── commands_discovery.rs ─ scan tmp ~/.claude/skills с SKILL.md → CommandInfo[]

crates/roy-gateway/tests/
└── ws_auth.rs          ─ subprotocol JWT → user_id в ctx; невалидный → 401 close
```

### Test fixtures

В `roy-auth` появляется `pub mod test_support` (cfg-флаг `test-support`):

```rust
pub async fn temp_pool() -> SqlitePool;     // in-memory sqlite + apply all migrations
pub async fn make_user(pool: &SqlitePool, username: &str) -> User;
pub async fn make_team(pool: &SqlitePool, owner: &User) -> Team;
pub fn issue_jwt(user_id: &str) -> String;  // sign_session с тест-secret'ом
```

Тест-`ROY_JWT_SECRET` — детерминированная константа в `test_support`.

### Что не покрывается юнит-тестами

| Не пишем | Почему |
|---|---|
| `bcrypt::hash`/`verify` сами по себе | Сторонняя библиотека. |
| Каждый axum-handler в изоляции | Покрывается через `auth_flow.rs`. |
| Cookie-парсинг | Сторонняя. |
| Bootstrap-root print в stderr | Проверяется косвенно (после старта есть user). |
| Cache TTL у `/commands` | Тривиально. |

### Регрессии

После рефакторинга должны проходить без модификаций:
- `crates/roy/tests/acp_transport.rs` — daemon не меняется.
- Интеграционные тесты `roy-scheduler` — wire-протокол не меняется.
- `cargo test --workspace --no-fail-fast` — CI gate.

### Ручной smoke-чеклист

```
[ ] roy-management стартует с ROY_JWT_SECRET, без — fail-fast.
[ ] Первый старт печатает bootstrap-password в stderr.
[ ] curl -c jar POST /auth/login + GET /auth/me — работает.
[ ] roy auth login (CLI) — пишет cookie в ~/.config/roy/cookie, whoami возвращает user.
[ ] WS-handshake с tampered JWT возвращает 401 close.
[ ] Создание session в team, где user не member — 403.
[ ] cwd для personal session = ~/.roy/workspace/users/<uid>/sessions/<sid>.
[ ] cwd для team+project = ~/.roy/workspace/teams/<tid>/projects/<pid>/sessions/<sid>.
[ ] GET /commands возвращает skill'ы из ~/.claude/skills.
[ ] cargo test --workspace --no-fail-fast — зелёный.
[ ] cargo fmt --all -- --check, cargo build --workspace --all-targets — зелёные.
```

## Конфигурация (env vars)

| Переменная | Назначение | Обязательная |
|---|---|---|
| `ROY_JWT_SECRET` | Секрет для подписи JWT (≥32 ASCII bytes) | да |
| `ROY_WORKSPACE_DIR` | Корень workspace-дерева (default `~/.roy/workspace`) | нет |
| `ROY_BOOTSTRAP_USERNAME` | Username первого пользователя (default `root`) | нет |
| `ROY_BOOTSTRAP_PASSWORD` | Пароль первого пользователя (default — generated, печатается в stderr) | нет |
| `ROY_HTTPS` | `1` → ставить `Secure` на cookie | нет |
| `ROY_TRUSTED_PROXIES` | Список IP/CIDR — доверять `X-Forwarded-For` | нет |

## Что НЕ меняется

- Wire-протокол daemon'а (`ClientCommand`/`ServerEvent`).
- `roy-cli` локальные подкоманды (`run`, `attach`, `list`, etc.) — продолжают ходить через Unix-сокет без JWT.
- `roy-scheduler` — зависит только от wire-протокола, не трогаем.
- `roy-agents` — таблица `agents` остаётся глобальной до отдельного будущего Plan C.
- Telegram-bridge — отдельная ось auth, не в scope.
- ACP-преcет конфигурация (`agents.toml`).
- Daemon trust model.

## Open questions

Нет. Все развилки закрыты в brainstorming:

1. **Где живут таблицы users/teams** → отдельный крейт `roy-auth`, shared `agents.db`.
2. **Как передаётся scope-контекст агенту** → только через `cwd`, никакого автоинджекта, никакой генерации `CLAUDE.md` от имени roy.
3. **JWT в HTTP и WS** → один формат повсюду, общий WS-токен удаляется.
4. **Commands** → discovery skill-файлов, без БД.
5. **Existing data** → нет переходного периода, миграция чистит `projects`/`session_meta`.
