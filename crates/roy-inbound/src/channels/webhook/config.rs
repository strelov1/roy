use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    pub path: String,
    #[serde(default)]
    pub secret_env: Option<String>,
    #[serde(default = "default_reply_mode")]
    pub reply_mode: ReplyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyMode {
    Sync,
    Async,
}

fn default_reply_mode() -> ReplyMode {
    ReplyMode::Sync
}
