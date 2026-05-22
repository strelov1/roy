use serde_json::Value;
use crate::event::TurnEvent;

/// One CLI's dialect. Pure logic: no process spawning, no I/O. Lets the
/// transport stay agent-agnostic and lets codex/gemini drop in later.
pub trait Provider: Send + Sync {
    /// Executable name, e.g. "claude".
    fn command(&self) -> &str;
    /// CLI args for a turn. `resume_cursor = None` => new session via
    /// `--session-id`; `Some(c)` => `--resume c`.
    fn spawn_args(&self, session_id: &str, resume_cursor: Option<&str>) -> Vec<String>;
    /// Encode one user message as a single stream-json line (newline-terminated).
    fn encode_user_message(&self, text: &str) -> String;
    /// Parse one stdout line into a normalized event, or None for noise.
    fn parse_line(&self, line: &str) -> Option<TurnEvent>;
    /// True when this event marks the end of the current turn.
    fn is_turn_end(&self, ev: &TurnEvent) -> bool;
}

pub struct ClaudeProvider {
    pub model: Option<String>,
}

impl ClaudeProvider {
    pub fn new(model: Option<String>) -> Self {
        Self { model }
    }
}

impl Provider for ClaudeProvider {
    fn command(&self) -> &str {
        "claude"
    }

    fn spawn_args(&self, session_id: &str, resume_cursor: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = vec!["-p".into()];
        match resume_cursor {
            Some(cursor) => {
                args.push("--resume".into());
                args.push(cursor.to_string());
            }
            None => {
                args.push("--session-id".into());
                args.push(session_id.to_string());
            }
        }
        args.push("--input-format".into());
        args.push("stream-json".into());
        args.push("--output-format".into());
        args.push("stream-json".into());
        args.push("--verbose".into());
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        args
    }

    fn encode_user_message(&self, text: &str) -> String {
        let msg = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
        });
        format!("{}\n", msg)
    }

    fn parse_line(&self, line: &str) -> Option<TurnEvent> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        match v.get("type").and_then(Value::as_str)? {
            "system" => {
                let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("").to_string();
                Some(TurnEvent::System { subtype })
            }
            "assistant" => {
                let content = v.get("message")?.get("content")?.as_array()?;
                // Prefer a text block; fall back to the first tool_use; skip
                // thinking-only lines. The authoritative final answer is the
                // `result` event, so dropping intermediate blocks is fine.
                for block in content {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        let text = block.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                        return Some(TurnEvent::AssistantText { text });
                    }
                }
                for block in content {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        return Some(TurnEvent::ToolUse { name, input });
                    }
                }
                None
            }
            "result" => {
                let cost_usd = v.get("total_cost_usd").and_then(Value::as_f64);
                let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                Some(TurnEvent::Result { cost_usd, is_error })
            }
            _ => Some(TurnEvent::Raw(v)),
        }
    }

    fn is_turn_end(&self, ev: &TurnEvent) -> bool {
        matches!(ev, TurnEvent::Result { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::TurnEvent;

    fn p() -> ClaudeProvider {
        ClaudeProvider { model: None }
    }

    #[test]
    fn parses_init() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc","cwd":"/tmp"}"#;
        assert_eq!(
            p().parse_line(line),
            Some(TurnEvent::System { subtype: "init".into() })
        );
    }

    #[test]
    fn parses_assistant_text() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#;
        assert_eq!(
            p().parse_line(line),
            Some(TurnEvent::AssistantText { text: "hello".into() })
        );
    }

    #[test]
    fn parses_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        match p().parse_line(line) {
            Some(TurnEvent::ToolUse { name, input }) => {
                assert_eq!(name, "Bash");
                assert_eq!(input["command"], "ls");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn thinking_only_assistant_is_skipped() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#;
        assert_eq!(p().parse_line(line), None);
    }

    #[test]
    fn parses_result_as_turn_end() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"hello","total_cost_usd":0.06}"#;
        let ev = p().parse_line(line).unwrap();
        assert_eq!(ev, TurnEvent::Result { cost_usd: Some(0.06), is_error: false });
        assert!(p().is_turn_end(&ev));
    }

    #[test]
    fn blank_and_garbage_lines_are_none() {
        assert_eq!(p().parse_line(""), None);
        assert_eq!(p().parse_line("   "), None);
        assert_eq!(p().parse_line("not json"), None);
    }

    #[test]
    fn unknown_type_becomes_raw() {
        let line = r#"{"type":"future_event","foo":1}"#;
        assert!(matches!(p().parse_line(line), Some(TurnEvent::Raw(_))));
    }

    #[test]
    fn encodes_user_message_as_single_json_line() {
        let line = p().encode_user_message("hi there");
        assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "hi there");
    }

    #[test]
    fn spawn_args_new_vs_resume() {
        let prov = ClaudeProvider { model: Some("claude-haiku-4-5-20251001".into()) };
        let new_args = prov.spawn_args("sid-1", None);
        assert!(new_args.contains(&"--session-id".to_string()));
        assert!(new_args.contains(&"sid-1".to_string()));
        assert!(new_args.contains(&"--model".to_string()));

        let resume_args = prov.spawn_args("sid-1", Some("cursor-9"));
        assert!(resume_args.contains(&"--resume".to_string()));
        assert!(resume_args.contains(&"cursor-9".to_string()));
        assert!(!resume_args.contains(&"--session-id".to_string()));
    }
}
