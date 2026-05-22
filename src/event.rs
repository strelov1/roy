use serde_json::Value;

/// Normalized event emitted during a turn. `Raw` preserves any line we don't
/// model yet so new event types don't get silently dropped.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    System { subtype: String },
    AssistantText { text: String },
    ToolUse { name: String, input: Value },
    Result { cost_usd: Option<f64>, is_error: bool },
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
    fn result_carries_cost_and_error_flag() {
        let e = TurnEvent::Result { cost_usd: Some(0.5), is_error: false };
        if let TurnEvent::Result { cost_usd, is_error } = e {
            assert_eq!(cost_usd, Some(0.5));
            assert!(!is_error);
        } else {
            panic!("wrong variant");
        }
    }
}
