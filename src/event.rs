use serde_json::{json, Value};

use crate::error::{Result, RoyError};

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

    /// Stable snake_case wire form. Mirrors the ACP `stopReason` vocabulary;
    /// `Other(s)` keeps an unmapped reason verbatim.
    pub fn as_wire(&self) -> &str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::MaxTurnRequests => "max_turn_requests",
            StopReason::Refusal => "refusal",
            StopReason::Cancelled => "cancelled",
            StopReason::Error => "error",
            StopReason::Other(s) => s.as_str(),
        }
    }

    /// Inverse of `as_wire`. Unknown strings become `Other(s)` rather than
    /// erroring — keeps the journal forward-compatible.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "max_turn_requests" => Self::MaxTurnRequests,
            "refusal" => Self::Refusal,
            "cancelled" => Self::Cancelled,
            "error" => Self::Error,
            other => Self::Other(other.to_string()),
        }
    }
}

/// JSON wire-format mapping for a `TurnEvent`. Single source of truth for CLI
/// stdout, the JSONL journal, and any future trigger protocol (Unix socket /
/// WebSocket). See `docs/superpowers/specs/2026-05-22-roy-cli-design.md`.
pub fn event_to_json(event: &TurnEvent) -> Value {
    match event {
        TurnEvent::System { subtype } => json!({"type": "system", "subtype": subtype}),
        TurnEvent::AssistantText { text } => json!({"type": "assistant_text", "text": text}),
        TurnEvent::ToolUse { name, input } => {
            json!({"type": "tool_use", "name": name, "input": input})
        }
        TurnEvent::Result {
            cost_usd,
            stop_reason,
        } => json!({
            "type": "result",
            "cost_usd": cost_usd,
            "stop_reason": stop_reason.as_wire(),
            "is_error": stop_reason.is_error(),
        }),
        TurnEvent::Raw(v) => json!({"type": "raw", "value": v}),
    }
}

/// Inverse of `event_to_json`. Used by the journal on replay.
pub fn event_from_json(v: &Value) -> Result<TurnEvent> {
    let ty = v
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| RoyError::Protocol(format!("event missing 'type': {v}")))?;
    match ty {
        "system" => Ok(TurnEvent::System {
            subtype: v
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "assistant_text" => Ok(TurnEvent::AssistantText {
            text: v
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "tool_use" => Ok(TurnEvent::ToolUse {
            name: v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            input: v.get("input").cloned().unwrap_or(Value::Null),
        }),
        "result" => Ok(TurnEvent::Result {
            cost_usd: v.get("cost_usd").and_then(Value::as_f64),
            stop_reason: StopReason::from_wire(
                v.get("stop_reason").and_then(Value::as_str).unwrap_or(""),
            ),
        }),
        "raw" => Ok(TurnEvent::Raw(v.get("value").cloned().unwrap_or(Value::Null))),
        other => Err(RoyError::Protocol(format!("unknown event type '{other}'"))),
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
