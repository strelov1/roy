//! Telegram support channel. Bots + agents are configured in `roy-management`;
//! this channel fetches them via the internal HTTP endpoint, runs one teloxide
//! dispatcher per bot pushing `InboundEvent`s onto the bus, and replies through
//! `TelegramReplyHook`. Per-sender sticky sessions live in the shared `bindings`
//! table (see `session.rs`).

pub mod reply;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use roy_protocol::channel::{SessionStrategyWire, TelegramSource};

use crate::session::SessionStrategy;
use reply::TgSender;

/// Runtime view of one bound bot, derived from a `TelegramSource`.
#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub source_id: String,
    pub agent_slug: String,
    pub harness: String,
    pub system_prompt: Option<String>,
    pub session_strategy: SessionStrategy,
    pub allowed_user_ids: Arc<Vec<i64>>,
    pub fire_timeout_secs: u64,
}

const DEFAULT_FIRE_TIMEOUT_SECS: u64 = 600;

impl From<TelegramSource> for ResolvedSource {
    fn from(s: TelegramSource) -> Self {
        let session_strategy = match s.session_strategy {
            SessionStrategyWire::Ephemeral => SessionStrategy::Ephemeral,
            SessionStrategyWire::PersistentOne => SessionStrategy::PersistentOne,
            SessionStrategyWire::PerSenderSticky { idle_timeout_secs } => {
                SessionStrategy::PerSenderSticky {
                    idle_timeout: Duration::from_secs(idle_timeout_secs),
                }
            }
        };
        ResolvedSource {
            source_id: s.source_id,
            agent_slug: s.agent_slug,
            harness: s.harness,
            system_prompt: s.system_prompt,
            session_strategy,
            allowed_user_ids: Arc::new(s.allowed_user_ids),
            fire_timeout_secs: DEFAULT_FIRE_TIMEOUT_SECS,
        }
    }
}

/// Shared registry of live Telegram sources. Synchronous lock so the reply-hook
/// factory (a sync closure) and the async router can both read it without await.
#[derive(Default)]
pub struct TelegramRegistry {
    inner: RwLock<HashMap<String, SourceRuntime>>,
}

pub(crate) struct SourceRuntime {
    pub resolved: Arc<ResolvedSource>,
    pub sender: Arc<dyn TgSender>,
    pub token: String,
    pub task: tokio::task::JoinHandle<()>,
}

impl TelegramRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn resolved_for(&self, source_id: &str) -> Option<Arc<ResolvedSource>> {
        self.inner
            .read()
            .unwrap()
            .get(source_id)
            .map(|r| r.resolved.clone())
    }

    pub fn sender_for(&self, source_id: &str) -> Option<Arc<dyn TgSender>> {
        self.inner
            .read()
            .unwrap()
            .get(source_id)
            .map(|r| r.sender.clone())
    }

    pub(crate) fn insert(&self, source_id: String, runtime: SourceRuntime) {
        if let Some(old) = self.inner.write().unwrap().insert(source_id, runtime) {
            old.task.abort();
        }
    }

    pub(crate) fn remove(&self, source_id: &str) {
        if let Some(old) = self.inner.write().unwrap().remove(source_id) {
            old.task.abort();
        }
    }

    pub fn source_ids(&self) -> Vec<String> {
        self.inner.read().unwrap().keys().cloned().collect()
    }
}

/// Thin HTTP client for `roy-management`'s internal source endpoint.
pub struct ManagementClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl ManagementClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
    }

    pub async fn fetch_telegram_sources(&self) -> Result<Vec<TelegramSource>> {
        let url = format!("{}/internal/telegram-sources", self.base_url);
        let resp = self.http.get(&url).bearer_auth(&self.token).send().await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("management returned {status} for telegram-sources");
        }
        Ok(resp.json::<Vec<TelegramSource>>().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_source_maps_sticky_strategy() {
        let src = TelegramSource {
            source_id: "tg:c1".into(),
            bot_token: "t".into(),
            agent_slug: "a".into(),
            harness: "claude".into(),
            system_prompt: Some("p".into()),
            model: None,
            session_strategy: SessionStrategyWire::PerSenderSticky {
                idle_timeout_secs: 60,
            },
            allowed_user_ids: vec![7],
        };
        let r: ResolvedSource = src.into();
        assert_eq!(r.harness, "claude");
        assert_eq!(r.system_prompt.as_deref(), Some("p"));
        assert!(matches!(
            r.session_strategy,
            SessionStrategy::PerSenderSticky { idle_timeout } if idle_timeout == Duration::from_secs(60)
        ));
        assert_eq!(*r.allowed_user_ids, vec![7]);
    }
}
