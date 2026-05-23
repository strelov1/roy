# roy — OpenCode support via the existing ACP transport (итерация 3)

Дата: 2026-05-22

## Цель

Добавить OpenCode как третий агент, переиспользуя существующий `AcpTransport`
без изменений транспорта/парсинга. OpenCode имеет подкоманду `opencode acp`,
которая поднимает ACP-сервер (JSON-RPC 2.0 по stdio) — тот же протокол, что у
gemini.

## Эмпирически подтверждено (opencode 1.15.7, не выводить заново)

Запуск: `opencode acp` (подкоманда, без флагов; `--skip-trust` не нужен).

```
→ initialize {protocolVersion:1, clientCapabilities:{}}
← {result:{protocolVersion:1, agentCapabilities:{loadSession:true,
     sessionCapabilities:{close,fork,list,resume}, ...}, authMethods:[opencode-login]}}
→ session/new {cwd, mcpServers:[]}
← {result:{sessionId:"ses_...", configOptions:[...]}}   ← НЕТ поля `modes`
→ session/prompt {sessionId, prompt:[{type:text,text}]}
← session/update agent_thought_chunk (несколько)        → TurnEvent::Raw
← session/update agent_message_chunk {content:{type:text,text:"hello"}} → AssistantText
← session/update usage_update                            → TurnEvent::Raw
← {result:{stopReason:"end_turn", usage:{...}}}          → Result(is_error:false) = конец хода
```

Ключевое отличие от gemini: **у opencode нет `modes`/`yolo`** (есть
`configOptions` для выбора модели). Поэтому `session/set_mode` слать НЕ нужно →
`mode_id = None`. Наш `AcpTransport::open` уже условный
(`if let Some(mode) = &self.config.mode_id { set_mode }`), так что при `None`
шаг просто пропускается.

Auth: opencode залогинен (`opencode auth login`); сессия отдала реальный ответ.
Permission: для текстового хода запросов не было; если придут — клиент уже
авто-одобряет `request_permission`.

Совместимость маппинга подтверждена: `agent_message_chunk`→`AssistantText`,
`agent_thought_chunk`/`usage_update`→`Raw`, `available_commands_update`→drop,
`stopReason`→`Result`. Изменения в `acp/protocol.rs` и `acp/client.rs` НЕ нужны.

## Изменения

1. `AcpConfig::opencode()` в `src/transport/acp/mod.rs`:
   ```rust
   pub fn opencode() -> Self {
       Self { command: "opencode".into(), args: vec!["acp".into()], mode_id: None }
   }
   ```
2. Hermetic-тест в `tests/acp_transport.rs`: открыть `AcpTransport` с
   `mode_id: None` против фейкового агента и проверить, что ход доходит до
   `Result` (путь «пропустить set_mode»).
3. `examples/demo_opencode.rs` — зеркало `demo_gemini` с `AcpConfig::opencode()`;
   зарегистрировать `[[example]]` в `Cargo.toml`.
4. ignored real-opencode smoke-тест в `tests/acp_transport.rs` (как
   `real_gemini_spawn_and_turn`, гейт по наличию `opencode` в PATH).

## Тестирование

- Hermetic: новый тест с `mode_id: None` (фейковый агент не получит set_mode).
- ignored smoke: реальный `opencode acp`, один ход, ответ содержит "hello",
  `resume_cursor` непустой.

## Контекст: t3code

t3code (форк пользователя) держит opencode через СВОЙ драйвер
(`OpenCodeDriver`/`opencodeRuntime`), а ACP-пакет использует для gemini/claude.
Для roy ACP-путь к opencode проще и единообразнее — отдельный драйвер не нужен.
Полезная идея на будущее (вне этой итерации): настраиваемый путь к бинарю
агента (t3code `binaryPath`).

## Явно вне итерации 3

Выбор модели через `configOptions`; настраиваемый `binaryPath`; claude через ACP.
