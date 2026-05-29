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
use async_trait::async_trait;
use chrono::Utc;
use roy_protocol::channel::{SessionStrategyWire, TelegramSource};
use serde_json::json;
use teloxide::prelude::*;
use teloxide::types::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::{BusSender, InboundEvent, ReplyHandle};
use crate::channels::Publisher;
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

/// Pure mapping from a Telegram message to an `InboundEvent` (unit-tested).
pub(crate) fn build_event(source_id: &str, chat_id: i64, user_id: i64, text: &str) -> InboundEvent {
    InboundEvent {
        id: Uuid::new_v4(),
        source_id: source_id.to_string(),
        source_kind: "telegram".into(),
        sender_id: chat_id.to_string(),
        payload: json!({ "text": text, "user_id": user_id }),
        received_at: Utc::now(),
        reply: ReplyHandle::Noop,
    }
}

/// Allowlist check: empty list = public.
pub(crate) fn allowed(allowed_ids: &[i64], user_id: i64) -> bool {
    allowed_ids.is_empty() || allowed_ids.contains(&user_id)
}

/// teloxide `Bot` wrapped as a `TgSender` for replies.
pub(crate) struct BotSender(pub teloxide::Bot);

#[async_trait]
impl reply::TgSender for BotSender {
    async fn send(&self, chat_id: i64, text: &str) -> anyhow::Result<()> {
        self.0
            .send_message(teloxide::types::ChatId(chat_id), text)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct TgDeps {
    source_id: Arc<str>,
    allowed: Arc<Vec<i64>>,
    bus: BusSender,
}

async fn on_message(msg: &Message, deps: &TgDeps) -> anyhow::Result<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0 as i64;
    if !allowed(&deps.allowed, user_id) {
        return Ok(());
    }
    let chat_id = msg.chat.id.0;
    let ev = build_event(&deps.source_id, chat_id, user_id, text);
    deps.bus
        .send(ev)
        .await
        .map_err(|_| anyhow::anyhow!("bus closed"))?;
    Ok(())
}

fn spawn_bot_task(
    bot: teloxide::Bot,
    source_id: Arc<str>,
    allowed_ids: Arc<Vec<i64>>,
    bus: BusSender,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let deps = TgDeps {
            source_id,
            allowed: allowed_ids,
            bus,
        };
        let handler = Update::filter_message().endpoint(
            |_bot: teloxide::Bot, msg: Message, deps: TgDeps| async move {
                if let Err(e) = on_message(&msg, &deps).await {
                    tracing::warn!(?e, "telegram on_message failed");
                }
                respond(())
            },
        );
        Dispatcher::builder(bot, handler)
            .dependencies(dptree::deps![deps])
            .build()
            .dispatch()
            .await;
    })
}

/// Publisher for the Telegram channel: fetches sources from `roy-management`,
/// runs one teloxide dispatcher per bot, and keeps the shared registry current.
pub struct TelegramPublisher {
    registry: Arc<TelegramRegistry>,
    client: Arc<ManagementClient>,
}

impl TelegramPublisher {
    pub fn new(registry: Arc<TelegramRegistry>, client: Arc<ManagementClient>) -> Self {
        Self { registry, client }
    }

    /// Build a bot, spawn its dispatcher, and insert it into the registry.
    fn start_source(&self, src: TelegramSource, bus: &BusSender) {
        let token = src.bot_token.clone();
        let resolved: ResolvedSource = src.into();
        let source_id: Arc<str> = Arc::from(resolved.source_id.as_str());
        let bot = teloxide::Bot::new(&token);
        let sender: Arc<dyn TgSender> = Arc::new(BotSender(bot.clone()));
        let task = spawn_bot_task(
            bot,
            source_id.clone(),
            resolved.allowed_user_ids.clone(),
            bus.clone(),
        );
        self.registry.insert(
            resolved.source_id.clone(),
            SourceRuntime {
                resolved: Arc::new(resolved),
                sender,
                token,
                task,
            },
        );
    }
}

#[async_trait]
impl Publisher for TelegramPublisher {
    async fn run(self: Arc<Self>, bus: BusSender, cancel: CancellationToken) -> Result<()> {
        // Initial load (slice 2: fetch once; slice 3 adds the poll loop).
        match self.client.fetch_telegram_sources().await {
            Ok(sources) => {
                tracing::info!(count = sources.len(), "telegram: starting bots");
                for src in sources {
                    self.start_source(src, &bus);
                }
            }
            Err(e) => tracing::error!(error = ?e, "telegram: initial source fetch failed"),
        }
        cancel.cancelled().await;
        for id in self.registry.source_ids() {
            self.registry.remove(&id);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use reply::NoopSender;

    /// Insert a resolved source with no live bot/task — for router/unit tests.
    pub fn insert_resolved(reg: &Arc<TelegramRegistry>, resolved: ResolvedSource) {
        let source_id = resolved.source_id.clone();
        reg.inner.write().unwrap().insert(
            source_id,
            SourceRuntime {
                resolved: Arc::new(resolved),
                sender: Arc::new(NoopSender),
                token: String::new(),
                task: tokio::spawn(async {}),
            },
        );
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

    #[test]
    fn build_event_shapes_payload() {
        let ev = build_event("tg:c1", 555, 999, "hi there");
        assert_eq!(ev.source_kind, "telegram");
        assert_eq!(ev.source_id, "tg:c1");
        assert_eq!(ev.sender_id, "555");
        assert_eq!(ev.payload["text"], "hi there");
        assert_eq!(ev.payload["user_id"], 999);
        assert!(matches!(ev.reply, crate::bus::ReplyHandle::Noop));
    }

    #[test]
    fn allowlist_logic() {
        assert!(allowed(&[], 5)); // empty = public
        assert!(allowed(&[5, 6], 5));
        assert!(!allowed(&[5, 6], 7));
    }
}
