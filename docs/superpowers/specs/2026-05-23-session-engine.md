# roy — движок сессий (registry + attach + journal)

Дата: 2026-05-23

## Цель

Перевести roy от одно-консьюмерной модели `Session::send → TurnStream` к
**движку сессий**, в котором:

- сессия живёт независимо от подключённых клиентов (фоновая задача — норма),
- **N наблюдателей** могут подписаться на полный нормализованный поток
  `TurnEvent` через всю жизнь сессии,
- **один** клиент в каждый момент держит lease на ввод (`send_prompt`),
- каждое событие пишется в per-session **JSONL-журнал** на диске и в памяти,
  с монотонным `seq`,
- медленный/опоздавший подписчик догоняется через журнал — агент не тормозится,
- движок транспорт-агностичный: не меняет `Transport`/`Handle`, строится
  **поверх** них.

WebSocket-адаптер — следующая итерация поверх этого движка.

## Архитектурный контекст

Сейчас: `Session::new(transport, cwd).send(prompt) -> TurnStream`. Один
потребитель, эфемерно — стрим живёт от prompt до `Result`. Транспорт
(`PrintTransport`/`AcpTransport`) уже владеет долгоживущим `Handle` через
один live-процесс.

Движок строится **над** `Handle`: переиспользует `Handle::send(prompt) ->
TurnStream`, прогоняет события через журнал и `tokio::sync::broadcast`. Никаких
изменений в trait'ах `Transport`/`Handle`. Существующий `Session` остаётся как
удобный частный случай для one-shot use.

## Структура модулей (в крейте `roy`)

```
src/
  journal.rs   - JSONL-журнал + memory-окно
  engine.rs    - SessionEngine + actor task
  manager.rs   - SessionManager (реестр)
  lib.rs       - pub use journal::*, engine::*, manager::*
```

## Контракты

### `Journal`

```rust
pub type Seq = u64;

pub struct JournalEntry {
    pub seq: Seq,
    pub event: TurnEvent,
}

pub struct Journal { /* private */ }

impl Journal {
    /// Opens <dir>/<session_id>.jsonl in append mode; loads tail into memory.
    pub async fn open(dir: &Path, session_id: &str, mem_capacity: usize)
        -> Result<Self>;

    /// Append + return assigned seq. Single-writer invariant (the engine actor).
    pub async fn append(&mut self, event: TurnEvent) -> Result<Seq>;

    /// Return entries with seq >= from_seq. Reads from memory if window covers
    /// it; otherwise streams from disk.
    pub async fn replay_from(&self, from_seq: Seq) -> Result<Vec<JournalEntry>>;
}
```

Файловый формат — одна JSON-строка на запись (см. ниже).

### `SessionEngine`

```rust
pub struct EngineOpts {
    pub journal_dir: PathBuf,
    pub broadcast_capacity: usize,   // bounded; lagging subscribers get Lagged
    pub mem_capacity: usize,         // in-memory ring window
}

pub struct SessionEngine { /* private */ }

impl SessionEngine {
    pub async fn spawn(
        transport: Arc<dyn Transport>,
        cwd: PathBuf,
        opts: EngineOpts,
    ) -> Result<Arc<Self>>;

    pub fn id(&self) -> &str;
    pub fn resume_cursor(&self) -> Option<String>;

    /// Subscribe; replay from journal then continue live, race-free.
    pub async fn attach(&self, from_seq: Option<Seq>) -> Result<Attach>;

    /// Returns None if another lease is alive. Drop releases.
    pub fn try_acquire_input(&self) -> Option<InputLease<'_>>;

    /// Graceful shutdown: drain queue, close Handle, finalize journal.
    pub async fn close(&self) -> Result<()>;
}

pub struct Attach {
    pub seq_at_attach: Seq,
    pub stream: Pin<Box<dyn Stream<Item = JournalEntry> + Send>>,
}

pub struct InputLease<'a> { /* released on drop */ }

impl InputLease<'_> {
    pub async fn send(&self, prompt: &str) -> Result<()>; // queue a turn
}
```

### `SessionManager`

```rust
pub struct SessionManager { /* private */ }

impl SessionManager {
    pub fn new(journal_dir: PathBuf) -> Self;

    pub async fn spawn(
        &self,
        transport: Arc<dyn Transport>,
        cwd: PathBuf,
        opts: EngineOpts,
    ) -> Result<Arc<SessionEngine>>;

    pub fn list(&self) -> Vec<String>;
    pub fn get(&self, id: &str) -> Option<Arc<SessionEngine>>;
    pub async fn close(&self, id: &str) -> Result<()>;
}
```

## JSONL-формат журнала

Один JSON-объект на строку:

```json
{"seq": 0, "event": {"type": "system", "subtype": "init"}}
{"seq": 1, "event": {"type": "assistant_text", "text": "hi"}}
{"seq": 2, "event": {"type": "result", "cost_usd": null, "stop_reason": "end_turn", "is_error": false}}
```

Маппинг `TurnEvent → event` идентичен `event_to_json` из roy-cli спеки. Тот же
wire-формат используют CLI, журнал и будущий WS-адаптер — единый контракт.

## Актор внутри движка

Один tokio-task на сессию:

1. `let mut handle = transport.open(session_id, resume, cwd).await?;`
2. Цикл по input-queue:
   - `Input::Prompt(text)`:
     1. `let mut stream = handle.send(&text).await?;`
     2. Пока `Some(ev) = stream.next().await`:
        `let seq = journal.append(ev.clone()).await?;`
        `let _ = broadcast.send(JournalEntry { seq, event: ev });`
   - `Input::Close`: `handle.close().await?;` → break.

Поток terminator'а — терминальный `TurnEvent::Result` — попадает в журнал и
в broadcast как обычное событие. Граница хода маркируется им же; никаких
дополнительных «end-of-turn» меток нет.

## `attach()` — race-free join

1. **Сначала** `let mut sub = broadcast.subscribe()` — фиксируем точку входа в
   live-потоке (буфер `broadcast_capacity` начинает копиться).
2. **Затем** прочитать `journal.replay_from(from_seq.unwrap_or(0))` в `Vec`.
3. Вернуть `Attach.stream` = реплей-чанк затем live-чанк, с фильтром на
   `seq > last_replayed_seq` чтобы не задублировать.
4. Если в live-чанке прилетел `RecvError::Lagged(n)` — это не fatal:
   подписчик/обёртка переподписывается и реплеит журнал от своего `last_seq`.
   Сама обёртка в `Attach.stream` делает это прозрачно.

## Lifecycle, ввод и отмена

- Сессия завершается **только** на `engine.close()` или `manager.close(id)`. Без
  idle-таймаута в v1.
- Input — **FIFO-очередь** в акторе. ACP-ходы последовательны (одновременных
  prompt'ов протокол не допускает) — это естественная семантика.
- Drop `InputLease` **не отменяет идущий ход**. Lease — только гейт на ввод.
  Отмена — отдельная фича со своими гарантиями (consistency mid-turn,
  взаимодействие с `session/cancel`) — следующая итерация.

## Backpressure

- `broadcast` ограничен `broadcast_capacity`. Медленный подписчик получает
  `RecvError::Lagged(n)`.
- Обёртка `Attach.stream` ловит `Lagged` и **переподписывается + догоняется
  через журнал** с последнего видимого `seq`. Прозрачно для клиента: он видит
  непрерывную последовательность `JournalEntry` с монотонным `seq`.
- Агент не тормозится никогда; durability — на журнале.

## Что НЕ меняем

- `Transport`, `Handle`, `TurnEvent`, `Session` — без правок.
- Существующие транспорты (`PrintTransport`, `AcpTransport`) не трогаем.
- `Session::send` остаётся — используется как удобный one-shot путь поверх
  существующего `Handle`. Можно реализовать через `SessionEngine` позже как
  тонкую обёртку, но это не цель v1.

## Демо

`examples/engine_two_attach.rs`:

1. `SessionManager::new(journal_dir)`.
2. `manager.spawn(AcpTransport::new(AcpConfig::opencode()), cwd, EngineOpts {..}).await?;`
3. Запустить два task'а `attach(None)`, каждый печатает свой поток с префиксом
   `A:`/`B:`.
4. Из третьего task'а взять `try_acquire_input()` и отправить две команды
   подряд.
5. Дождаться двух `Result` в обоих наблюдателях, `manager.close(id).await?`.

Это и есть приёмочный тест архитектуры.

## Тестирование

- Юнит `journal`: `append` → `replay_from` через границу memory/disk
  (memory-окно меньше числа записей).
- Юнит `engine`: `try_acquire_input` exclusivity (второй вызов даёт `None`,
  drop первого возвращает доступ).
- Интеграция `engine_basic` поверх `fake-acp-agent.py`: spawn → два `attach` →
  отправить два промпта → оба подписчика получают идентичные seq-стримы вплоть
  до двух `Result`-событий.
- Интеграция `engine_slow_attach`: одного подписчика искусственно тормозим
  (`sleep`), проверяем что второй не блокируется, после `Lagged` медленный
  догоняется через журнал и получает все `seq` без пропусков.

## Roadmap → server + triggers (следующие итерации)

Движок сессий — это **библиотечный** API. Над ним будет жить **один демон**,
который владеет единственным `SessionManager` на пользователя и принимает
команды от **триггеров** — тонких адаптеров под разные framings/среды.

```
┌──────────────────────────────────────────────────────┐
│  roy serve  (daemon, one per user)                    │
│  ────────────                                         │
│  • Arc<SessionManager>  (in-process owner всех        │
│    живых сессий и журналов)                           │
│  • набор listener'ов, говорящих общим control-        │
│    протоколом (JSON-framed):                          │
│      ▸ Unix socket  ~/.roy/daemon.sock  (CLI/MCP)     │
│      ▸ WebSocket    --port <p>          (WS клиенты)  │
│      ▸ HTTP/REST                          — позже     │
│      ▸ ...                                 без правок │
│                                            ядра       │
└──────────────────────────────────────────────────────┘
       ▲              ▲              ▲              ▲
       │              │              │              │
    roy run       WS client    cron / queue     MCP host
   (UNIX sock)   (browser)       (HTTP)         (Unix sock)
```

### Решения, фиксируем сейчас

- **Сервер embed'ит `SessionManager`** (не шеллит в `roy run`). Иначе теряется
  единый реестр и журнал с многосторонним attach: stdout отдельных child-
  процессов не fanout-ится. Crash-isolation отдаём сознательно;
  fault-tolerance — через журнал + будущий resurrect.
- **Один демон на пользователя.** PID-файл + фиксированный socket
  `~/.roy/daemon.sock`. Без флагов клиентов «куда подключаться» нет — упрощает
  default-flow; кастомные пути возможны через `--socket`.
- **CLI без демона — ошибка с подсказкой.** `roy run`/`roy attach`/etc. без
  активного `roy serve` пишут в stderr `no daemon at ~/.roy/daemon.sock — start
  it with \`roy serve\`` и exit 2. Никаких эмбед-fallback'ов в default-режиме —
  одна точка правды.

### Control-протокол (один на все триггеры)

Различается только framing: длиной-префиксированные JSON-фреймы поверх Unix
socket, тексто-JSON фреймы поверх WS. Семантика операций одна.

| op | поля | действие |
|----|------|---------|
| `spawn` | `agent`, `cwd?`, `model?` | `SessionManager::spawn` → `{session, resume_cursor}` |
| `attach` | `session`, `from_seq?` | `SessionEngine::attach` |
| `acquire_input` | `session` | взять exclusive input-lease |
| `send` | `session`, `text` | через активный lease |
| `release_input` | `session` | отпустить lease |
| `detach` | `session` | отписать только этот коннект, сессия живёт |
| `close` | `session` | `SessionManager::close` |
| `list` | — | `SessionManager::list` |

События сервер → клиент: `{"session": "<id>", "entry": {"seq": N, "event":
<event_to_json>}}` — тот же `event_to_json`, что в CLI stdout и в JSONL-журнале.
**Один поперечный wire-контракт** покрывает CLI stdout, JSONL-журнал,
Unix-socket фреймы и WS-фреймы — `tail -f <id>.jsonl` и WS-клиент говорят на
одном языке.

### Триггеры на старте

- **Unix socket trigger** — слушает `~/.roy/daemon.sock`, длиной-префиксированные
  JSON-фреймы. Главный путь для локальных клиентов (CLI, MCP-host'ы).
- **WebSocket trigger** — слушает `--port` (опционально, по умолчанию выключен),
  тексто-JSON фреймы. Для удалённых/браузерных клиентов. Один WS-коннект
  мультиплексирует N подписок на разные сессии (поле `session` в каждом фрейме).

Новые триггеры (HTTP/REST, MCP, gRPC) добавляются как ещё один listener поверх
того же control-протокола без правок ядра.

### Фоновые агенты — частный случай

«Фоновый агент» в этой модели — сессия, у которой клиент сделал `spawn` +
`acquire_input` + `send` + `detach` и вышел. Сессия живёт, журнал пишется,
позже кто угодно делает `attach(from_seq: 0)` и видит всё. Никаких отдельных
примитивов background в API нет.

## Вне объёма (YAGNI)

- WebSocket-адаптер (отдельный модуль/крейт следующей итерацией).
- Воскрешение сессий после рестарта roy (журнал переживает, но процесс агента
  нет; resurrection через `session/load` ACP — отдельная фича).
- Передача input lease между клиентами и TTL.
- Cancel на drop input-lease (требует семантики `session/cancel` +
  взаимодействия с журналом).
- Rotation / compaction журнала.
- M:N запись (несколько одновременных писателей).
- Метрики и трейсинг.
