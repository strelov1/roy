# roy — Gemini support via ACP transport (итерация 2)

Дата: 2026-05-22

## Цель

Добавить поддержку Gemini как второго агента, **без respawn на каждый ход**.
Gemini CLI не умеет принимать сообщения через stdin-стрим (`--input-format`
отсутствует), а `-p` — одноразовый. Зато `gemini --acp` поднимает
**персистентный процесс, говорящий по Agent Client Protocol (ACP)** — JSON-RPC
2.0 поверх stdio: один живой процесс, ходы шлются как `session/prompt`,
события приходят `session/update`-нотификациями. Это даёт дешёвый multi-turn
без перезапуска и без PTY.

Объём итерации: текст + multi-turn + resume + auto-approve (yolo). Без
attachments, без выбора модели, без интерактивного OAuth.

## Эмпирически подтверждённый протокол (gemini 0.43.0, не выводить заново)

Запуск: `gemini --acp --skip-trust`. stderr содержит шум (YOLO/Ripgrep/Skill),
поэтому читаем только stdout — там чистый JSON-RPC построчно (по одному
объекту на строку).

Поток одной сессии (client → agent, если не указано иное):

```
→ {"jsonrpc":"2.0","id":1,"method":"initialize",
    "params":{"protocolVersion":1,"clientCapabilities":{}}}
← {"id":1,"result":{"protocolVersion":1,"authMethods":[...],
    "agentCapabilities":{"loadSession":true,"promptCapabilities":{...}}}}

→ {"id":2,"method":"session/new","params":{"cwd":"<abs>","mcpServers":[]}}
← {"id":2,"result":{"sessionId":"<uuid>","modes":{"availableModes":[
    {"id":"default"},{"id":"yolo"},...],"currentModeId":"default"},"models":{...}}}

→ {"id":3,"method":"session/set_mode","params":{"sessionId":"<uuid>","modeId":"yolo"}}
← {"id":3,"result":{}}

→ {"id":4,"method":"session/prompt","params":{"sessionId":"<uuid>",
    "prompt":[{"type":"text","text":"<prompt>"}]}}
← (notification) {"method":"session/update","params":{"sessionId":"<uuid>",
    "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}}
← {"id":4,"result":{"stopReason":"end_turn","_meta":{"quota":{"token_count":{...}}}}}  ← конец хода
```

Resume существующей сессии (вместо `session/new`):
```
→ {"id":2,"method":"session/load","params":{"sessionId":"<uuid>","cwd":"<abs>","mcpServers":[]}}
```
`agentCapabilities.loadSession == true` — поддерживается.

Прочие сообщения, которые могут прийти от агента:
- notification `session/update` с `sessionUpdate` = `available_commands_update`,
  `tool_call`, `tool_call_update`, `plan`, `agent_thought_chunk` и др.
- request `session/request_permission` (id + params) — отвечаем
  `{"result":{"outcome":{"outcome":"selected","optionId":"allow"}}}`. При
  `modeId:"yolo"` инструменты и так авто-одобряются, но обработчик нужен на
  случай прихода запроса.

Auth: gemini уже залогинен (oauth-personal). Если `session/new` вернёт ошибку
авторизации — отдаём понятный `RoyError`, советующий запустить `gemini` и
выполнить вход. Полный интерактивный OAuth НЕ реализуем.

## Архитектура: стабильное ядро + второй транспорт

`Session` и `TurnEvent` — стабильное ядро, не меняются по смыслу. Расширяем ось
транспорта.

```
Session  (держит Arc<dyn Transport> + cwd + resume_cursor)
   │   send(prompt) -> Stream<TurnEvent>
   │
   ├── PrintTransport   (claude)  — живой процесс, stdin stream-json   [есть]
   └── AcpTransport     (gemini)  — живой процесс, JSON-RPC/ACP по stdio [новый]
```

### Рефакторинг: убрать Provider из сигнатуры Transport

Сейчас `Transport::open(provider, session_id, resume_cursor, cwd)` тащит
line-`Provider`, который ACP не нужен. Это делаем чисто:

- `Transport::open(&self, session_id, resume_cursor, cwd) -> Result<Box<dyn Handle>>`
  — без `provider`.
- Каждый транспорт инкапсулирует свою конфигурацию при конструировании:
  - `PrintTransport::new(provider: Arc<dyn Provider>)` — держит `ClaudeProvider`.
  - `AcpTransport::new(config: AcpConfig)` — держит команду/аргументы запуска.
- `Session::new(transport: Arc<dyn Transport>, cwd)` и
  `Session::resume(transport, cwd, session_id)` — больше НЕ принимают provider.

`Provider` остаётся как деталь `PrintTransport` (диалект claude). `TurnEvent`,
`Handle`, `Session` API наружу — те же.

### Структура файлов (рефакторинг transport.rs → модуль)

`src/transport.rs` растёт; разбиваем на модуль с одним назначением на файл:

- `src/transport/mod.rs` — трейты `Transport` + `Handle`, ре-экспорт
  `PrintTransport`, `AcpTransport`.
- `src/transport/print.rs` — `PrintTransport`/`PrintHandle` (перенос текущего
  кода, минус `provider` из сигнатуры open).
- `src/transport/acp/mod.rs` — `AcpTransport`/`AcpHandle` (реализация трейтов).
- `src/transport/acp/protocol.rs` — serde-типы ACP-сообщений (Initialize,
  SessionNew, SessionLoad, SetMode, Prompt, SessionUpdate, ContentBlock,
  RequestPermission) и преобразование `SessionUpdate -> TurnEvent`.
- `src/transport/acp/client.rs` — `JsonRpcClient`: владеет stdin-писателем,
  счётчиком id, картой ожидающих ответов; фоновая reader-задача разбирает
  stdout и маршрутизирует ответы/нотификации/входящие запросы.

`AcpConfig` (в `acp/mod.rs`):
```rust
pub struct AcpConfig {
    pub command: String,        // "gemini"
    pub args: Vec<String>,      // ["--acp", "--skip-trust"]
    pub mode_id: Option<String>,// Some("yolo")
}
impl AcpConfig {
    pub fn gemini() -> Self { /* command="gemini", args=["--acp","--skip-trust"], mode_id=Some("yolo") */ }
}
```

## JSON-RPC клиент (`acp/client.rs`)

ACP — двунаправленный JSON-RPC-пир. Клиент должен:

- **Слать запросы** с автоинкрементным `id` и ждать ответ
  (`request(method, params) -> Future<Result<Value>>` через `oneshot`-канал,
  ключ = id в `HashMap<i64, oneshot::Sender<...>>`, под `Mutex`).
- **Слать нотификации** (без id).
- **Слать ответы** на входящие запросы агента.
- **Фоновая reader-задача**: на каждую строку stdout — `serde_json` парс →
  - есть `id` + (`result`|`error`) → ответ: достать sender из карты, разрешить.
  - есть `method` без `id` → нотификация: если `session/update` — конвертнуть в
    `TurnEvent` и отправить в активный turn-канал (`mpsc::Sender<TurnEvent>`,
    хранится в общем состоянии, ставится на время хода).
  - есть `method` + `id` → запрос агента: если `*request_permission*` — ответить
    `allow`; иначе вернуть JSON-RPC error `method not found`.
  - не-JSON / пустые строки → пропуск.

Разрешение хода: `session/prompt` шлём как обычный запрос; его `Future`
завершается ответом со `stopReason`. Параллельно `session/update`-нотификации
текут в turn-канал. По ответу на `session/prompt` reader-задача шлёт в
turn-канал финальный `TurnEvent::Result` и закрывает канал.

## AcpHandle: open / send

```rust
// AcpTransport::open(session_id, resume_cursor, cwd):
//   1. spawn config.command + config.args, cwd
//   2. запустить JsonRpcClient (reader-задачу)
//   3. initialize {protocolVersion:1, clientCapabilities:{}}
//   4. resume_cursor: Some(sid) -> session/load {sid, cwd, mcpServers:[]}
//                     None      -> session/new  {cwd, mcpServers:[]} -> sid
//      (session/new НЕ принимает наш session_id; ACP сам выдаёт sessionId.
//       resume_cursor = выданный sessionId.)
//   5. если config.mode_id задан -> session/set_mode {sid, mode_id}
//   -> AcpHandle { client, session_id_acp }
//
// AcpHandle::send(prompt):
//   1. поставить turn-канал (mpsc) в общее состояние клиента
//   2. отправить session/prompt {sessionId, prompt:[{type:"text",text:prompt}]}
//      как запрос; его ответ обработает reader (-> Result в turn-канал)
//   3. вернуть Stream, читающий turn-канал до TurnEvent::Result
//
// AcpHandle::close(): отправить session/cancel при активном ходе (опц.), убить процесс.
```

Важно про resume_cursor: ACP сам генерирует `sessionId` в `session/new`. Поэтому
`Session.id` (UUID, который мы придумали) для ACP **не** совпадает с ACP
sessionId. `resume_cursor` для gemini = ACP `sessionId`, выданный агентом.
`Session` уже хранит `resume_cursor` отдельно от `id` — после первого хода
проставляем `resume_cursor = <ACP sessionId>` (а не `self.id`, как делает
PrintTransport для claude). Значит обновление `resume_cursor` должно идти ОТ
транспорта, а не зашиваться в `Session`.

### Уточнение Session: курсор приходит от транспорта

Сейчас `Session::send` после первого open делает `resume_cursor = Some(self.id)`.
Это верно для claude (id == claude session id), но НЕ для gemini (id != ACP sid).
Чистое решение: `Handle` сообщает свой курсор.

```rust
#[async_trait]
pub trait Handle: Send {
    async fn send(&mut self, prompt: &str) -> Result<Pin<Box<dyn Stream<Item=TurnEvent> + Send + '_>>>;
    /// Непрозрачный токен для возобновления ЭТОЙ сессии при следующем open.
    /// claude: переданный session_id. gemini: ACP sessionId из session/new.
    fn resume_cursor(&self) -> Option<String>;
    async fn close(&mut self) -> Result<()>;
}
```

`Session::send` после open берёт `resume_cursor` из `handle.resume_cursor()`.
PrintHandle возвращает `Some(session_id)`; AcpHandle — `Some(<ACP sessionId>)`.

## Маппинг ACP -> TurnEvent (`acp/protocol.rs`)

- `session/update` `agent_message_chunk`, `content.type=="text"`
  -> `TurnEvent::AssistantText { text }` (чанки-дельты; отдаём по мере прихода).
- `session/update` `tool_call` -> `TurnEvent::ToolUse { name, input }`
  (`name` из `title`/`kind`, `input` из `rawInput`/`content`; чего нет — `Null`).
- ответ `session/prompt` -> `TurnEvent::Result { cost_usd: None, is_error }`,
  где `is_error = stopReason != "end_turn" && stopReason != "max_tokens"`.
  (В ACP нет долларовой стоимости — только токены в `_meta.quota`; cost_usd=None.)
- прочие `sessionUpdate` (`available_commands_update`, `plan`,
  `agent_thought_chunk`, `tool_call_update`) -> `TurnEvent::Raw(<update json>)`
  или пропуск (`available_commands_update` -> пропуск как шум).
- факт открытия сессии -> `TurnEvent::System { subtype: "session_new" | "session_load" }`
  (эмитим один раз в open, чтобы дать parity с claude `init`).

## Обработка ошибок

- `gemini` не установлен -> `RoyError::Spawn`.
- `initialize`/`session/new` вернул JSON-RPC error -> `RoyError` с текстом ошибки;
  если похоже на auth -> подсказка «запустите gemini и войдите».
- процесс закрыл stdout до конца хода -> turn-канал закрывается, `send` отдаёт
  ошибку через `RoyError::ProcessExited`.
- таймаут хода (дефолт как в claude) -> `RoyError::Timeout`, процесс убивается.
- неизвестные входящие запросы агента -> JSON-RPC error «method not found»
  (не роняем процесс).

## Тестирование

- **Hermetic (без gemini):** фейковый ACP-агент — маленький Python-скрипт
  (`tests/scripts/fake-acp-agent.py`), который реализует минимальный
  JSON-RPC: отвечает на initialize/session-new/set_mode, на session/prompt шлёт
  пару `agent_message_chunk`-нотификаций и ответ со `stopReason:"end_turn"`.
  Интеграционный тест поднимает `AcpTransport` с командой этого скрипта и
  проверяет: open -> send -> Stream содержит AssistantText + завершается Result;
  второй send в том же процессе (multi-turn); `resume_cursor()` непустой.
  Отдельный тест: фейковый агент шлёт `session/request_permission` -> проверяем,
  что транспорт авто-отвечает `allow` и ход доходит до Result.
- **Unit:** `acp/protocol.rs` — маппинг `SessionUpdate`/prompt-result -> TurnEvent
  на строках-фикстурах (реальные сообщения gemini из этой спеки).
- **Unit:** `acp/client.rs` — корреляция запрос/ответ по id, маршрутизация
  нотификаций (можно через in-memory pipe без процесса).
- **Ignored smoke (реальный gemini):** spawn -> "reply with exactly: hello" ->
  AssistantText == "hello"; затем resume того же sessionId -> второй ход.
  Гейтится наличием `gemini` в PATH.

## Крейты (добавить к существующим)

Новых тяжёлых зависимостей не требуется: `serde`/`serde_json` уже есть,
`tokio` (process/io/sync) есть, `async-stream` есть. Для карты ожидающих
ответов — `std::collections::HashMap` под `tokio::sync::Mutex`. Возможно
`futures` (dev) уже подключён.

## Явно вне итерации 2

Claude через ACP (нативно не поддерживает; нужен внешний adapter); выбор модели
через ACP (`session/set_model`); attachments (image/audio); интерактивный OAuth;
fs-capability клиента (`fs/read_text_file`/`write_text_file` — не анонсируем,
агент использует свой доступ); план/мысли как отдельные события; токен-стоимость.
