use serde::Deserialize;
use serde_json::Value;

use crate::event::TurnEvent;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionUpdateParams {
    update: SessionUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionUpdate {
    session_update: String,
    #[serde(default)]
    content: Option<ContentBlock>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default, rename = "rawInput")]
    raw_input: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

/// Map an ACP `session/update` params object to a TurnEvent, or None to drop
/// (noise / unmodeled). `Raw` preserves unknown update kinds.
pub fn update_to_event(params: &Value) -> Option<TurnEvent> {
    let raw_update = params.get("update")?.clone();
    let params: SessionUpdateParams = serde_json::from_value(params.clone()).ok()?;
    match params.update.session_update.as_str() {
        "agent_message_chunk" => {
            let ContentBlock::Text { text } = params.update.content? else {
                return None;
            };
            Some(TurnEvent::AssistantText { text })
        }
        "tool_call" => {
            let name = params
                .update
                .title
                .or(params.update.kind)
                .unwrap_or_default();
            let input = params.update.raw_input.unwrap_or(Value::Null);
            Some(TurnEvent::ToolUse { name, input })
        }
        "available_commands_update" => None,
        _ => Some(TurnEvent::Raw(raw_update)),
    }
}

/// Map a `session/prompt` result object to the terminal Result event.
pub fn prompt_result_to_event(result: &Value) -> TurnEvent {
    let stop = result
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("");
    let is_error = !(stop == "end_turn" || stop == "max_tokens");
    TurnEvent::Result {
        cost_usd: None,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_agent_message_chunk_text() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}"#,
        ).unwrap();
        assert_eq!(
            update_to_event(&p),
            Some(TurnEvent::AssistantText {
                text: "hello".into()
            })
        );
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
        assert_eq!(
            prompt_result_to_event(&r),
            TurnEvent::Result {
                cost_usd: None,
                is_error: false
            }
        );
    }

    #[test]
    fn prompt_result_refusal_is_error() {
        let r: Value = serde_json::from_str(r#"{"stopReason":"refusal"}"#).unwrap();
        assert_eq!(
            prompt_result_to_event(&r),
            TurnEvent::Result {
                cost_usd: None,
                is_error: true
            }
        );
    }
}
