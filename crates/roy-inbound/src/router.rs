//! Router turns an InboundEvent into a FireSpec. Default ConfigRouter
//! looks up source_id in the loaded config, renders the template, and
//! builds the tag map.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::bus::{InboundEvent, TAG_PREFIX};
use crate::channels::telegram::TelegramRegistry;
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

/// Router for Telegram sources resolved from `roy-management`.
pub struct TelegramRouter {
    registry: Arc<TelegramRegistry>,
}

impl TelegramRouter {
    pub fn new(registry: Arc<TelegramRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Router for TelegramRouter {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec> {
        let src = self.registry.resolved_for(&ev.source_id)?;
        let prompt = ev
            .payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let mut tags = BTreeMap::new();
        tags.insert(format!("{TAG_PREFIX}:source_id"), ev.source_id.clone());
        tags.insert(format!("{TAG_PREFIX}:source_kind"), ev.source_kind.clone());
        tags.insert(format!("{TAG_PREFIX}:event_id"), ev.id.to_string());
        tags.insert(format!("{TAG_PREFIX}:sender_id"), ev.sender_id.clone());
        Some(FireSpec {
            agent_id: src.agent_slug.clone(),
            prompt,
            session_strategy: src.session_strategy.clone(),
            tags,
            fire_timeout_secs: src.fire_timeout_secs,
            harness: Some(src.harness.clone()),
            system_prompt: src.system_prompt.clone(),
        })
    }
}

/// Routes `telegram` events through `TelegramRouter`, everything else through
/// `ConfigRouter` (webhook).
pub struct CompositeRouter {
    pub telegram: TelegramRouter,
    pub config: ConfigRouter,
}

#[async_trait]
impl Router for CompositeRouter {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec> {
        if ev.source_kind == "telegram" {
            self.telegram.route(ev).await
        } else {
            self.config.route(ev).await
        }
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

    fn tg_ev(source_id: &str, text: &str) -> InboundEvent {
        InboundEvent {
            id: Uuid::new_v4(),
            source_id: source_id.into(),
            source_kind: "telegram".into(),
            sender_id: "555".into(),
            payload: serde_json::json!({ "text": text }),
            received_at: chrono::Utc::now(),
            reply: ReplyHandle::Noop,
        }
    }

    fn reg_insert_for_test(
        reg: &Arc<TelegramRegistry>,
        source_id: &str,
        harness: &str,
        prompt: Option<&str>,
    ) {
        use crate::channels::telegram::test_support::insert_resolved;
        use crate::channels::telegram::ResolvedSource;
        insert_resolved(
            reg,
            ResolvedSource {
                source_id: source_id.into(),
                agent_slug: "agent-x".into(),
                harness: harness.into(),
                system_prompt: prompt.map(str::to_string),
                session_strategy: SessionStrategy::Ephemeral,
                allowed_user_ids: Arc::new(vec![]),
                fire_timeout_secs: 600,
            },
        );
    }

    #[tokio::test]
    async fn telegram_router_builds_spec_with_persona() {
        let reg = TelegramRegistry::new();
        let tg = TelegramRouter::new(reg.clone());
        // No source registered → None.
        assert!(tg.route(&tg_ev("tg:c1", "hi")).await.is_none());

        // Register a resolved source directly (bypassing teloxide).
        reg_insert_for_test(&reg, "tg:c1", "claude", Some("persona"));
        let spec = tg.route(&tg_ev("tg:c1", "hi")).await.unwrap();
        assert_eq!(spec.prompt, "hi");
        assert_eq!(spec.harness.as_deref(), Some("claude"));
        assert_eq!(spec.system_prompt.as_deref(), Some("persona"));
        assert_eq!(spec.agent_id, "agent-x");
    }
}
