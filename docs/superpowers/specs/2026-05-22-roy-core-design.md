# roy — core (итерация 1)

Дата: 2026-05-22

## Цель

Минимальное Rust-ядро, которое умеет **spawn сессии и resume** для CLI-агентов,
начиная с Claude Code. Ядро отдаёт ход как **поток событий** (`Stream<TurnEvent>`)
и держит сессию живой для multi-turn.

Спроектировать так, чтобы:
- другие CLI (codex, gemini) добавлялись как новые реализации `Provider`;
- интерактивный PTY-транспорт добавлялся как новая реализация `Transport`,
не трогая сессионный слой.

## Контекст и обоснование

Референс `claude-agent` (`bridge/claude-runner.ts`) гоняет claude через
persistent PTY ради подписочной тарификации: его `-p`/`--print` берёт деньги
из отдельного пула кредитов, а интерактивный TUI — из подписки. Это даёт
работающий, но тяжёлый механизм: эмуляция терминала (xterm headless),
Stop-хук + sentinel-файл, поллинг JSONL-транскрипта, bracketed-paste ввод,
idle-таймеры.

Для **итерации 1** мы сознательно НЕ берём этот механизм. Claude Code
поддерживает headless-режим со стримингом:

- `claude -p --output-format stream-json --verbose` — JSONL-поток событий в stdout;
- `claude -p --input-format stream-json` — подача user-сообщений в stdin живого
процесса в реальном времени;
- `--session-id <uuid>` / `--resume <id>` — фиксация/возобновление сессии;
- `--include-partial-messages`, `--replay-user-messages` — токен-стриминг и ack.

Это даёт **persistent multi-turn без PTY**: процесс
`claude -p --input-format stream-json --output-format stream-json` живёт,
сообщения пишутся в stdin, события читаются из stdout. Намного проще, чем
PTY+sentinel+транскрипт.

Тарификационная оговорка: claude `-p` берёт из отдельного пула. Для прототипа
ядра это приемлемо; PTY-транспорт ради корректного биллинга claude добавим
итерацией 2. Для codex/gemini раздельной тарификации headless-режима нигде не
заявлено — для них print-транспорт это постоянный путь.

## Архитектура: две оси абстракции

```
roy::session    Session + SessionManager. Хранит id + resume_cursor.
                send(prompt) -> Stream<TurnEvent>. Один путь для spawn/resume.
   │
   ├── roy::transport   trait Transport — КАК гоняем байты
   │     • PrintTransport (итер.1): -p, stdin/stdout stream-json, процесс живёт
   │     • PtyTransport   (итер.2): persistent PTY ради биллинга claude
   │
   └── roy::provider    trait Provider — диалект конкретного CLI
         • ClaudeProvider (итер.1): команда, print-аргументы, нормализация
           событий claude-JSON -> TurnEvent, кодирование user-сообщения,
           распознавание "result" = конец хода
         • Codex/Gemini (позже): свои реализации
```

`Transport` спрашивает у `Provider` команду/аргументы/парсер/кодировщик.
Добавить codex/gemini = новый `Provider`. Добавить PTY = новый `Transport`.
Сессионный слой не меняется.

### Изоляция модулей

- `roy::provider` — чистая логика диалекта CLI: строит аргументы, кодирует
  user-сообщение в одну stream-json строку, парсит строку вывода в `TurnEvent`,
  отвечает на вопрос «это конец хода?». Без I/O, без процессов.
  Тестируется на строках-фикстурах без запуска claude.
- `roy::transport` — владеет процессом/потоками I/O. Не знает про конкретный CLI,
  спрашивает всё у `Provider`. `PrintTransport` спавнит процесс, пишет в stdin,
  построчно читает stdout, прогоняет строки через `provider.parse_line`.
- `roy::session` — хранит идентичность сессии и `resume_cursor`; решает spawn vs
  resume; не знает деталей транспорта/провайдера сверх трейтов.

## Типы и интерфейсы (эскиз)

```rust
// Нормализованное событие хода. Raw сохраняет исходный JSON для отладки/forward-compat.
pub enum TurnEvent {
    System { subtype: String },
    AssistantText { text: String },
    ToolUse { name: String, input: serde_json::Value },
    Result { cost_usd: Option<f64>, is_error: bool },
    Raw(serde_json::Value),
}

pub trait Provider: Send + Sync {
    fn command(&self) -> &str;                                // "claude"
    fn spawn_args(&self, s: &Session, resume: bool) -> Vec<String>;
    fn encode_user_message(&self, text: &str) -> String;      // одна stream-json строка + '\n'
    fn parse_line(&self, line: &str) -> Option<TurnEvent>;    // None для шума/пустых строк
    fn is_turn_end(&self, ev: &TurnEvent) -> bool;            // claude: TurnEvent::Result
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(&self, provider: Arc<dyn Provider>, session: &Session) -> Result<Box<dyn Handle>>;
}

#[async_trait]
pub trait Handle: Send {
    // Пишет prompt в живой процесс, возвращает поток событий до конца хода (is_turn_end).
    async fn send(&mut self, prompt: &str) -> Result<BoxStream<'_, TurnEvent>>;
    async fn close(&mut self) -> Result<()>;
}

pub struct Session {
    pub id: String,                       // UUID, он же claude --session-id
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub resume_cursor: Option<String>,    // непрозрачен для ядра; claude кладёт session-id
}
```

### Поведение

- **spawn** (новая сессия): `PrintTransport.open` спавнит
  `claude -p --session-id <uuid> --input-format stream-json
  --output-format stream-json --verbose [--model M] [-C cwd]`; процесс живёт.
- **send(prompt)**: пишем `provider.encode_user_message(prompt)` в stdin, читаем
  stdout построчно, прогоняем через `provider.parse_line`, отдаём `TurnEvent` в
  поток; поток завершается, когда `provider.is_turn_end(ev)`. Процесс остаётся
  живым → следующий `send` это multi-turn в том же процессе.
- **resume**: если `resume_cursor` задан и процесса нет — `open` со spawn-аргументами
  через `--resume <cursor>` вместо `--session-id`. Иначе как spawn.
- После первого хода `session.resume_cursor = Some(session.id)`.

## Обработка ошибок

- Процесс упал/закрыл stdout до конца хода → поток завершается, `send` возвращает
  ошибку «process exited mid-turn» с хвостом stderr.
- Невалидная JSON-строка в stdout → `parse_line` возвращает `None` (пропуск), не
  роняем ход (forward-compat к новым типам событий).
- Таймаут хода (конфигурируемый, дефолт 10 мин) → ошибка, процесс убивается.
- На границе ядра используем `anyhow::Result`; доменные ошибки — `thiserror`.

## Тестирование

- `provider`: unit-тесты на фикстурах строк stream-json claude (взять реальный
  вывод `claude -p --output-format stream-json` как фикстуру) → проверяем
  `parse_line` и `is_turn_end`. Без запуска процесса.
- `transport`: интеграционный тест с фейковым «провайдером», команда которого —
  маленький скрипт, эмитящий заранее заданные JSON-строки → проверяем, что
  `send`/поток/конец хода работают без реального claude.
- smoke-тест (ignored по умолчанию, требует установленного claude): реальный
  spawn + один ход + resume.

## Крейты

`tokio` (process, io, time), `tokio-stream`/`futures` (Stream), `serde`,
`serde_json`, `uuid`, `async-trait`, `anyhow`, `thiserror`.
`portable-pty` НЕ нужен в итерации 1.

## Явно вне итерации 1

PTY-транспорт; codex/gemini-провайдеры; HTTP/SSE-сервер; abort/interrupt;
attachments; approvals; персистентность сессий на диск; корректный
подписочный биллинг claude; токен-партиалы (`--include-partial-messages`).
```
