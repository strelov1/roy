//! Router turns an InboundEvent into a FireSpec. Default ConfigRouter
//! looks up source_id in the loaded config, renders the template, and
//! builds the tag map.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::bus::{InboundEvent, TAG_PREFIX};
use crate::config::{InboundConfig, SourceConfig};
use crate::session::SessionStrategy;
use crate::template::render;

#[derive(Debug, Clone)]
pub struct FireSpec {
    pub agent_id: String,
    pub prompt: String,
    pub session_strategy: SessionStrategy,
    pub tags: BTreeMap<String, String>,
    pub fire_timeout_secs: u64,
    /// Per-source harness override (Telegram channel). `None` → resolver default.
    pub harness: Option<String>,
    /// Per-source system/persona prompt (Telegram channel). `None` → no prompt.
    pub system_prompt: Option<String>,
}

#[async_trait]
pub trait Router: Send + Sync {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec>;
}

pub struct ConfigRouter {
    sources_by_id: BTreeMap<String, Arc<SourceConfig>>,
}

impl ConfigRouter {
    pub fn from_config(cfg: &InboundConfig) -> Self {
        let sources_by_id = cfg
            .sources
            .iter()
            .map(|s| (s.id.clone(), Arc::new(s.clone())))
            .collect();
        Self { sources_by_id }
    }
}

#[async_trait]
impl Router for ConfigRouter {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec> {
        let src = self.sources_by_id.get(&ev.source_id)?;
        let prompt = render(&src.template, &ev.payload);
        let mut tags = BTreeMap::new();
        tags.insert(format!("{TAG_PREFIX}:source_id"), ev.source_id.clone());
        tags.insert(format!("{TAG_PREFIX}:source_kind"), ev.source_kind.clone());
        tags.insert(format!("{TAG_PREFIX}:event_id"), ev.id.to_string());
        tags.insert(format!("{TAG_PREFIX}:sender_id"), ev.sender_id.clone());
        Some(FireSpec {
            agent_id: src.agent_id.clone(),
            prompt,
            session_strategy: SessionStrategy::from(&src.session),
            tags,
            fire_timeout_secs: src.fire_timeout_secs,
            harness: None,
            system_prompt: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{InboundEvent, ReplyHandle};
    use serde_json::json;
    use uuid::Uuid;

    fn event(source_id: &str, payload: serde_json::Value) -> InboundEvent {
        InboundEvent {
            id: Uuid::new_v4(),
            source_id: source_id.into(),
            source_kind: "webhook".into(),
            sender_id: "alice".into(),
            payload,
            received_at: chrono::Utc::now(),
            reply: ReplyHandle::Noop,
        }
    }

    fn cfg(toml: &str) -> InboundConfig {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, toml).unwrap();
        InboundConfig::load(&p).unwrap()
    }

    #[tokio::test]
    async fn unknown_source_returns_none() {
        let c = cfg(r#"
            [[sources]]
            id = "orders"
            kind = "webhook"
            agent_id = "bot"
            session = "ephemeral"
            template = "x"
            [sources.webhook]
            path = "/o"
            reply_mode = "sync"
        "#);
        let r = ConfigRouter::from_config(&c);
        assert!(r.route(&event("not-orders", json!({}))).await.is_none());
    }

    #[tokio::test]
    async fn known_source_renders_template_and_tags() {
        let c = cfg(r#"
            [[sources]]
            id = "orders"
            kind = "webhook"
            agent_id = "bot"
            session = "ephemeral"
            template = "Order {{payload.id}}"
            [sources.webhook]
            path = "/o"
            reply_mode = "sync"
        "#);
        let r = ConfigRouter::from_config(&c);
        let ev = event("orders", json!({"id": 42}));
        let spec = r.route(&ev).await.unwrap();
        assert_eq!(spec.agent_id, "bot");
        assert_eq!(spec.prompt, "Order 42");
        assert_eq!(spec.tags["roy-inbound:source_id"], "orders");
        assert_eq!(spec.tags["roy-inbound:sender_id"], "alice");
        assert!(matches!(spec.session_strategy, SessionStrategy::Ephemeral));
    }
}
