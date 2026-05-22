use serde_json::Value;

use crate::event::TurnEvent;

/// Map an ACP `session/update` params object to a TurnEvent, or None to drop
/// (noise / unmodeled). `Raw` preserves unknown update kinds.
pub fn update_to_event(params: &Value) -> Option<TurnEvent> {
    let update = params.get("update")?;
    match update.get("sessionUpdate").and_then(Value::as_str)? {
        "agent_message_chunk" => {
            let content = update.get("content")?;
            if content.get("type").and_then(Value::as_str) == Some("text") {
                let text = content.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                Some(TurnEvent::AssistantText { text })
            } else {
                None
            }
        }
        "tool_call" => {
            let name = update
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| update.get("kind").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
            Some(TurnEvent::ToolUse { name, input })
        }
        "available_commands_update" => None,
        _ => Some(TurnEvent::Raw(update.clone())),
    }
}

/// Map a `session/prompt` result object to the terminal Result event.
pub fn prompt_result_to_event(result: &Value) -> TurnEvent {
    let stop = result.get("stopReason").and_then(Value::as_str).unwrap_or("");
    let is_error = !(stop == "end_turn" || stop == "max_tokens");
    TurnEvent::Result { cost_usd: None, is_error }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_agent_message_chunk_text() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}"#,
        ).unwrap();
        assert_eq!(update_to_event(&p), Some(TurnEvent::AssistantText { text: "hello".into() }));
    }

    #[test]
    fn drops_available_commands_update() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"available_commands_update","availableCommands":[]}}"#,
        ).unwrap();
        assert_eq!(update_to_event(&p), None);
    }

    #[test]
    fn maps_tool_call() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"tool_call","title":"Bash","rawInput":{"command":"ls"}}}"#,
        ).unwrap();
        match update_to_event(&p) {
            Some(TurnEvent::ToolUse { name, input }) => {
                assert_eq!(name, "Bash");
                assert_eq!(input["command"], "ls");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn unknown_update_is_raw() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}}"#,
        ).unwrap();
        assert!(matches!(update_to_event(&p), Some(TurnEvent::Raw(_))));
    }

    #[test]
    fn prompt_result_end_turn_is_success() {
        let r: Value = serde_json::from_str(r#"{"stopReason":"end_turn"}"#).unwrap();
        assert_eq!(prompt_result_to_event(&r), TurnEvent::Result { cost_usd: None, is_error: false });
    }

    #[test]
    fn prompt_result_refusal_is_error() {
        let r: Value = serde_json::from_str(r#"{"stopReason":"refusal"}"#).unwrap();
        assert_eq!(prompt_result_to_event(&r), TurnEvent::Result { cost_usd: None, is_error: true });
    }
}
