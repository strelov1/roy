# roy — CLI для спауна агентов с заданиями (workspace + roy-cli)

Дата: 2026-05-22

## Цель

Дать тонкий CLI поверх существующей либы `roy`: один запуск = один агент + одна
задача, события хода стримятся в stdout как JSON Lines (для программного
потребления родительским процессом, который «спаунит агентов»). Вся логика уже
в либе — CLI добавляет только разбор аргументов, маппинг имени агента на
transport и проекцию `TurnEvent` в JSON.

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

## Интерфейс CLI

```
roy run <agent> <task> [--cwd <path>] [--model <id>]
                        [--permission allow|deny] [--pretty] [--resume <cursor>]
```

- `<agent>` ∈ `claude | gemini | opencode | codex | claude-agent`.
- Парсинг — `clap` (derive). `run` — подкоманда (оставляет место под будущее без
  ломки совместимости; сейчас единственная).
- Без `--resume` → `Session::new(transport, cwd)`. С `--resume <cursor>` →
  `Session::resume_with_cursor(...)`.

### Маппинг агента на transport

| `<agent>`      | transport |
|----------------|-----------|
| `claude`       | `PrintTransport::new(ClaudeProvider::new(model))` |
| `gemini`       | `AcpTransport::new(AcpConfig::gemini())` |
| `opencode`     | `AcpTransport::new(AcpConfig::opencode())` |
| `codex`        | `AcpTransport::new(AcpConfig::codex())` |
| `claude-agent` | `AcpTransport::new(AcpConfig::claude_agent())` |

## Валидация флагов (fail-fast, без тихого игнора)

- `--model` валиден только для `claude`. Для ACP-агентов → ошибка CLI.
- `--permission allow|deny` валиден только для ACP-агентов; переопределяет
  `AcpConfig.permission_policy` (`allow`→`AllowAll`, `deny`→`Deny`). Для `claude`
  → ошибка CLI.

Следует code quality bar репо: явная ошибка вместо молча проигнорированного флага.

## Поток выполнения (`main.rs`)

1. Распарсить аргументы (clap).
2. Провалидировать совместимость флагов с выбранным агентом.
3. Собрать transport по маппингу.
4. `Session::new` или `Session::resume_with_cursor`.
5. `session.send(task)` → для каждого `TurnEvent`: вывести строку в stdout
   (`event_to_json` в JSONL-режиме, либо human-принтер в `--pretty`), флашить.
6. После завершения стрима — финальная строка с курсором:
   `{"type":"session","resume_cursor":"..."}` (в `--pretty`: `resume_cursor = ...`).
7. `session.close()`, выставить exit-code.

## JSON wire-формат (вариант A — живёт в `roy-cli`)

`fn event_to_json(&TurnEvent) -> serde_json::Value`. Либа не получает `Serialize`
на `TurnEvent` — формат stdout это контракт CLI.

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

## Коды выхода

- `0` — терминальный `Result` с не-error `stop_reason` (`EndTurn`/`MaxTokens`).
- `1` — `Result` с error `stop_reason` (refusal/cancelled/error/...).
- `2` — ошибка самого CLI (неизвестный агент, провал спавна, плохие аргументы,
  несовместимые флаги). Сообщение в stderr; stdout остаётся чистым JSONL.

## Тестирование

- Юнит: `event_to_json` для всех вариантов (включая `Raw` с не-map значением).
- Юнит: валидация флагов (`--model`+ACP → ошибка; `--permission`+claude → ошибка).
- Интеграция: запустить собранный бинарь `roy` против существующих fake-агентов
  (`crates/roy/tests/scripts/fake-agent.sh`, `fake-acp-agent.py`), проверить
  JSONL-вывод и exit-code. Fake-агенты переиспользуются, новых не добавляем.

## Вне объёма (YAGNI)

- Интерактивный REPL / многоходовость в одном запуске.
- Чтение нескольких задач из файла / батч-оркестрация.
- Параллельный запуск агентов.
- `WireEvent`-тип в либе и derive `Serialize` на `TurnEvent`.
