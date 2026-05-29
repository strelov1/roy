//! Wire DTOs for inbound channels managed by `roy-management` and consumed by
//! `roy-inbound`. Control-plane only (config), never session operations.

use serde::{Deserialize, Serialize};

/// One Telegram bot resolved to its agent persona. Returned by
/// `roy-management`'s `GET /internal/telegram-sources` and consumed by the
/// `roy-inbound` Telegram channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramSource {
    /// Stable per-bot id: `"tg:<connection_id>"`.
    pub source_id: String,
    /// Plain Telegram Bot API token, e.g. `"123456:ABC-DEF..."`.
    pub bot_token: String,
    /// Agent slug (record-keeping; stored in the runtime binding).
    pub agent_slug: String,
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub session_strategy: SessionStrategyWire,
    /// Empty = public (any Telegram user may message the bot).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_user_ids: Vec<i64>,
}

/// Wire form of the per-source session strategy (mirrors
/// `roy_inbound::session::SessionStrategyConfig`).
///
/// `roy-management` and `roy-inbound` are deployed in lockstep from the same
/// workspace, so an unknown strategy is a hard deserialize error by design —
/// no `Other`/catch-all variant is needed.
///
/// Conversion points: `roy-management`'s `strategy_to_wire` (producer) and
/// `roy-inbound`'s `ResolvedSource::from` (consumer → runtime `SessionStrategy`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionStrategyWire {
    Ephemeral,
    PersistentOne,
    PerSenderSticky { idle_timeout_secs: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_source_round_trips() {
        let src = TelegramSource {
            source_id: "tg:conn-1".into(),
            bot_token: "123:abc".into(),
            agent_slug: "support-l1".into(),
            harness: "claude".into(),
            system_prompt: Some("You are support.".into()),
            session_strategy: SessionStrategyWire::PerSenderSticky {
                idle_timeout_secs: 3600,
            },
            allowed_user_ids: vec![],
        };
        let json = serde_json::to_string(&src).unwrap();
        // empty + None fields omitted
        assert!(!json.contains("allowed_user_ids"));
        assert!(json.contains(r#""kind":"per_sender_sticky""#));
        let back: TelegramSource = serde_json::from_str(&json).unwrap();
        assert_eq!(src, back);
    }

    #[test]
    fn strategy_short_variants_use_kind_tag() {
        let j = serde_json::to_string(&SessionStrategyWire::Ephemeral).unwrap();
        assert_eq!(j, r#"{"kind":"ephemeral"}"#);
    }

    #[test]
    fn persistent_one_round_trips() {
        let j = serde_json::to_string(&SessionStrategyWire::PersistentOne).unwrap();
        assert_eq!(j, r#"{"kind":"persistent_one"}"#);
        let back: SessionStrategyWire = serde_json::from_str(&j).unwrap();
        assert_eq!(back, SessionStrategyWire::PersistentOne);
    }
}
