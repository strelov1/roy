//! Render a stream of `TurnEvent`s into a growing HTML body suitable for
//! Telegram's `parseMode: HTML`. Keeps a block-state machine so successive
//! `AssistantText` (or `AssistantThought`) deltas extend the same block,
//! and any other event finalizes the active block.

use roy::event::TurnEvent;
use serde_json::Value;
use teloxide::utils::html::escape;

#[derive(Debug, Default)]
struct ActiveBlock {
    kind: BlockKind,
    buf: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum BlockKind {
    #[default]
    None,
    Text,
    Thought,
}

#[derive(Debug, Default)]
pub struct Renderer {
    finalized: Vec<String>, // each entry is one full block, already HTML-escaped and wrapped
    active: ActiveBlock,
}

impl Renderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event. Mutates internal state; the rendered body is read via `body()`.
    pub fn feed(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::AssistantText { text } => self.extend_block(BlockKind::Text, &text),
            TurnEvent::AssistantThought { text } => self.extend_block(BlockKind::Thought, &text),
            TurnEvent::ToolUse { name, input } => {
                self.finalize_active();
                let args_str = render_tool_args(&input);
                self.finalized.push(format!(
                    "🔧 <code>{}({})</code>",
                    escape(&name),
                    escape(&args_str)
                ));
            }
            TurnEvent::System { subtype } => {
                self.finalize_active();
                self.finalized
                    .push(format!("<i>ℹ {}</i>", escape(&subtype)));
            }
            TurnEvent::Usage {
                input_tokens,
                output_tokens,
                cost_usd,
            } => {
                self.finalize_active();
                let tokens = input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0);
                self.finalized.push(format!(
                    "📊 <code>tokens={} cost=${:.4}</code>",
                    tokens,
                    cost_usd.unwrap_or(0.0)
                ));
            }
            TurnEvent::Raw(value) => {
                self.finalize_active();
                let compact = serde_json::to_string(&value).unwrap_or_default();
                self.finalized
                    .push(format!("⚙ <code>{}</code>", escape(&compact)));
            }
            TurnEvent::UserPrompt { .. } | TurnEvent::Result { .. } => {
                // UserPrompt is our own input echoed back; not rendered.
                // Result is terminal and handled by the caller's pipeline.
            }
        }
    }

    /// Append an explicit error footer line (for terminal `Result` with error stop_reason).
    pub fn append_error_footer(&mut self, reason: &str) {
        self.finalize_active();
        self.finalized.push(format!("⚠ {}", escape(reason)));
    }

    /// Return the current rendered body, joining finalized blocks and the active one.
    pub fn body(&self) -> String {
        let mut all: Vec<String> = self.finalized.clone();
        if let Some(active) = self.render_active() {
            all.push(active);
        }
        all.join("\n\n")
    }

    fn extend_block(&mut self, kind: BlockKind, delta: &str) {
        if self.active.kind != kind {
            self.finalize_active();
            self.active.kind = kind;
            self.active.buf.clear();
        }
        self.active.buf.push_str(delta);
    }

    fn finalize_active(&mut self) {
        if let Some(rendered) = self.render_active() {
            self.finalized.push(rendered);
        }
        self.active.kind = BlockKind::None;
        self.active.buf.clear();
    }

    fn render_active(&self) -> Option<String> {
        if self.active.buf.is_empty() {
            return None;
        }
        let escaped = escape(&self.active.buf);
        Some(match self.active.kind {
            BlockKind::None => escaped,
            BlockKind::Text => escaped,
            BlockKind::Thought => format!("🧠 thinking: <i>{}</i>", escaped),
        })
    }
}

const TOOL_ARGS_MAX: usize = 200;

fn render_tool_args(args: &Value) -> String {
    let raw = serde_json::to_string(args).unwrap_or_default();
    if raw.len() <= TOOL_ARGS_MAX {
        raw
    } else {
        let safe = raw.floor_char_boundary(TOOL_ARGS_MAX);
        format!("{}…", &raw[..safe])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy::event::{StopReason, TurnEvent};
    use serde_json::json;

    #[test]
    fn text_deltas_concatenate_into_one_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantText {
            text: "Hello ".into(),
        });
        r.feed(TurnEvent::AssistantText {
            text: "world".into(),
        });
        assert_eq!(r.body(), "Hello world");
    }

    #[test]
    fn thought_deltas_render_inside_thinking_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantThought {
            text: "Let me ".into(),
        });
        r.feed(TurnEvent::AssistantThought {
            text: "check".into(),
        });
        assert_eq!(r.body(), "🧠 thinking: <i>Let me check</i>");
    }

    #[test]
    fn switching_kinds_finalizes_active_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantThought {
            text: "thinking".into(),
        });
        r.feed(TurnEvent::AssistantText {
            text: "answer".into(),
        });
        assert_eq!(r.body(), "🧠 thinking: <i>thinking</i>\n\nanswer");
    }

    #[test]
    fn tool_use_is_standalone_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantThought {
            text: "checking...".into(),
        });
        r.feed(TurnEvent::ToolUse {
            name: "read".into(),
            input: json!({"path": "main.rs"}),
        });
        r.feed(TurnEvent::AssistantText {
            text: "looks fine.".into(),
        });
        assert_eq!(
            r.body(),
            "🧠 thinking: <i>checking...</i>\n\n🔧 <code>read({\"path\":\"main.rs\"})</code>\n\nlooks fine."
        );
    }

    #[test]
    fn html_special_chars_escaped() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantText {
            text: "if a < b && c > d".into(),
        });
        assert_eq!(r.body(), "if a &lt; b &amp;&amp; c &gt; d");
    }

    #[test]
    fn long_tool_args_truncate() {
        let mut r = Renderer::new();
        let long = "x".repeat(500);
        r.feed(TurnEvent::ToolUse {
            name: "n".into(),
            input: json!({"big": long}),
        });
        let body = r.body();
        assert!(body.contains("…"));
        assert!(body.len() < 300);
    }

    #[test]
    fn long_tool_args_with_multibyte_does_not_panic() {
        let mut r = Renderer::new();
        // 500 'я' (2 bytes each) = 1000 bytes, well over TOOL_ARGS_MAX
        let long_val = "я".repeat(500);
        r.feed(TurnEvent::ToolUse {
            name: "n".into(),
            input: json!({"big": long_val}),
        });
        let body = r.body();
        assert!(body.contains("…"));
    }

    #[test]
    fn user_prompt_and_result_are_skipped() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::UserPrompt {
            text: "ignore me".into(),
        });
        r.feed(TurnEvent::AssistantText { text: "yo".into() });
        r.feed(TurnEvent::Result {
            cost_usd: None,
            stop_reason: StopReason::EndTurn,
        });
        assert_eq!(r.body(), "yo");
    }

    #[test]
    fn error_footer_appended_after_active_block() {
        let mut r = Renderer::new();
        r.feed(TurnEvent::AssistantText {
            text: "partial".into(),
        });
        r.append_error_footer("aborted");
        assert_eq!(r.body(), "partial\n\n⚠ aborted");
    }
}
