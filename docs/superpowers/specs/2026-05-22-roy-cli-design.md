# roy — CLI: триггер демона `roy serve` (workspace + roy-cli)

Дата: 2026-05-22 (пивот 2026-05-23 — CLI стал клиентом демона)

## Цель

CLI — это **тонкий триггер** демона `roy serve`, который держит все живые
сессии в одном `SessionManager` (см. `2026-05-23-session-engine.md`). CLI
разбирает аргументы, открывает Unix-сокет к демону, отправляет управляющие
операции и проецирует приходящие `JournalEntry` в stdout как JSON Lines.

`roy run` — primary one-shot путь. `roy serve` — поднять демон. `roy attach`/
`roy list`/`roy close` — управляющие команды.

## Структура репозитория (Cargo workspace)

```
roy/
├── Cargo.toml              # [workspace] members = ["crates/*"]
├── rustfmt.toml            # общий, остаётся в корне
├── docs/                   # остаётся в корне
├── crates/
│   ├── roy/                # текущая либа целиком (git mv, история сохраняется)
│   │   ├── Cargo.toml      # бывший корневой [package] name = "roy"
│   │   ├── src/
│   │   ├── tests/
│   │   └── examples/
│   └── roy-cli/
│       ├── Cargo.toml      # name = "roy-cli", [[bin]] name = "roy"
│       └── src/main.rs
```

- Перенос либы — `git mv src tests examples crates/roy/`, перемещение
  `Cargo.toml` → `crates/roy/Cargo.toml`. `[[example]]`-пути остаются
  относительными (`examples/...`) и продолжают работать.
- Корневой `Cargo.toml` становится чисто `[workspace]` (`resolver = "2"`).
- `Cargo.lock` остаётся в корне (workspace-уровень).
- `roy-cli` содержит **обе** роли — клиент (`run`/`attach`/`list`/`close`) и
  сервер (`serve`). Один бинарь, подкоманды; отдельный демон-бинарь не нужен.

## Подкоманды

```
roy serve  [--socket <path>] [--port <p>] [--journal-dir <dir>]
roy run    <agent> <task> [--cwd <path>] [--model <id>]
                          [--permission allow|deny] [--detach]
                          [--pretty] [--resume <cursor>]
roy attach <session> [--from-seq <n>] [--pretty]
roy list   [--json]
roy close  <session>
```

- `<agent>` ∈ `claude | gemini | opencode | codex | claude-agent`. Маппинг
  имени агента на `Transport` живёт **в демоне** (он владеет `SessionManager`),
  не в CLI. CLI передаёт только имя.
- Default Unix-сокет: `~/.roy/daemon.sock`. Переопределить — `--socket` у
  `roy serve` и `ROY_SOCKET` env для клиентских команд.
- Парсинг — `clap` (derive).

### `roy serve`

Поднимает демон с единственным `Arc<SessionManager>`. Всегда слушает Unix-сокет;
WS-listener включается только при `--port`. PID-файл рядом с сокетом для
проверки one-instance-per-user (попытка запуска при живом демоне — exit 2 с
подсказкой).

### `roy run`

One-shot путь через демон:

1. Подключиться к Unix-сокету. Если ответа нет — stderr
   `no daemon at <path> — start it with \`roy serve\``, exit 2.
2. Отправить control-операцию `spawn { agent, cwd, model? }` → получить
   `{session_id, resume_cursor}`. С `--resume <cursor>` — `attach_resume
   { cursor }` вместо `spawn`.
3. `acquire_input { session }` → `send { session, text: <task> }`.
4. Если `--detach` — напечатать
   `{"type":"session","id":...,"resume_cursor":...}` и выйти 0. Сессия живёт
   в демоне; позже её достают `roy attach <id>` или WS-клиентом.
5. Иначе — `attach { session }`, стримить `JournalEntry`-фреймы в stdout
   (через `event_to_json`). После терминального `Result` напечатать
   `{"type":"session","resume_cursor":"..."}` и выйти с кодом по `stop_reason`.

### `roy attach`

То же чтение журнала из демона, без `spawn`. `--from-seq N` — реплей с
указанного seq (опоздавшие/догон после `Lagged` в broadcast). Выходит после
ближайшего терминального `Result` или по SIGINT.

### `roy list` / `roy close`

Тривиальные обёртки над `list` / `close` операциями control-протокола.

## Валидация флагов (fail-fast, без тихого игнора)

- `--model` валиден только для `claude` (и `claude-agent` если поддерживается
  моделью). Для остальных ACP-агентов → ошибка CLI exit 2.
- `--permission allow|deny` валиден только для ACP-агентов; маппится на
  `AcpConfig.permission_policy` на стороне демона. Для `claude` → exit 2.
- `--detach` валиден только в `roy run`.
- `--from-seq` валиден только в `roy attach`.

Следует code quality bar репо: явная ошибка вместо молча проигнорированного флага.

## JSON wire-формат

`fn event_to_json(&TurnEvent) -> serde_json::Value`. Один маппинг — общий для
CLI stdout, JSONL-журнала, фреймов Unix-сокета и WS. Один контракт = одна точка
правки.

| TurnEvent | JSON |
|-----------|------|
| `System { subtype }` | `{"type":"system","subtype":...}` |
| `AssistantText { text }` | `{"type":"assistant_text","text":...}` |
| `ToolUse { name, input }` | `{"type":"tool_use","name":...,"input":...}` |
| `Result { cost_usd, stop_reason }` | `{"type":"result","cost_usd":...,"stop_reason":"end_turn","is_error":false}` |
| `Raw(v)` | `{"type":"raw","value":v}` |

- `stop_reason` → snake_case строка; `Other(s)` → сам `s`.
- `is_error` вычисляется из `stop_reason.is_error()`.
- `cost_usd` сериализуется как `null`, если `None`.

CLI печатает `event` из приходящего `JournalEntry { seq, event }`. Под флагом
`--with-seq` префиксует `{"seq":N,"event":...}` для машинного потребления
(полезно при `roy attach --from-seq` и реассемблинге).

## Коды выхода

- `0` — терминальный `Result` с не-error `stop_reason` (`EndTurn`/`MaxTokens`),
  или успешное завершение `serve`/`attach`/`list`/`close`.
- `1` — `Result` с error `stop_reason` (refusal/cancelled/error/...).
- `2` — ошибка самого CLI (нет демона, плохие аргументы, несовместимые флаги,
  неизвестный агент, демон уже запущен при `roy serve`). Сообщение в stderr;
  stdout остаётся чистым JSONL.

## Тестирование

- Юнит: `event_to_json` для всех вариантов (включая `Raw` с не-map значением).
- Юнит: валидация флагов (`--model`+ACP → ошибка; `--permission`+claude →
  ошибка; `--detach` вне `run` → ошибка; `--from-seq` вне `attach` → ошибка).
- Интеграция: поднять `roy serve` против fake-агентов
  (`crates/roy/tests/scripts/fake-agent.sh`, `fake-acp-agent.py`), прогнать
  `roy run` / `roy run --detach` / `roy attach` / `roy list` / `roy close`,
  проверить JSONL-вывод, exit-code и реестр. Fake-агенты переиспользуются,
  новых не добавляем.

## Вне объёма (YAGNI)

- Интерактивный REPL / многоходовость в одном `roy run`.
- Чтение нескольких задач из файла / батч-оркестрация.
- Параллельный спавн нескольких агентов из одной команды CLI.
- Auto-boot демона из клиентских команд.
- TLS на WS-listener.
- `WireEvent`-тип в либе и derive `Serialize` на `TurnEvent`.
