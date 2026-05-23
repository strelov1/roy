use serde_json::Value;

/// Why a turn ended. Normalized across agents (ACP stop reasons, claude result
/// subtypes). `Other` keeps an unmapped reason string instead of losing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    /// A failure not described by a clean protocol stop reason (process died,
    /// transport error, agent-side error result).
    Error,
    Other(String),
}

impl StopReason {
    /// True when the turn did not complete normally. `EndTurn`/`MaxTokens` are
    /// the only non-error outcomes.
    pub fn is_error(&self) -> bool {
        !matches!(self, StopReason::EndTurn | StopReason::MaxTokens)
    }
}

/// Normalized event emitted during a turn. `Raw` preserves any update we don't
/// model yet so new event types don't get silently dropped.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    System {
        subtype: String,
    },
    AssistantText {
        text: String,
    },
    ToolUse {
        name: String,
        input: Value,
    },
    Result {
        cost_usd: Option<f64>,
        stop_reason: StopReason,
    },
    Raw(Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_variants() {
        let e = TurnEvent::AssistantText { text: "hi".into() };
        match e {
            TurnEvent::AssistantText { text } => assert_eq!(text, "hi"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn result_carries_cost_and_stop_reason() {
        let e = TurnEvent::Result {
            cost_usd: Some(0.5),
            stop_reason: StopReason::EndTurn,
        };
        if let TurnEvent::Result {
            cost_usd,
            stop_reason,
        } = e
        {
            assert_eq!(cost_usd, Some(0.5));
            assert!(!stop_reason.is_error());
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn refusal_and_cancelled_are_errors() {
        assert!(StopReason::Refusal.is_error());
        assert!(StopReason::Cancelled.is_error());
        assert!(StopReason::Error.is_error());
        assert!(!StopReason::EndTurn.is_error());
        assert!(!StopReason::MaxTokens.is_error());
    }
}
