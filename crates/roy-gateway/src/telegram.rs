//! Teloxide bot loop: receive text DMs, route through the orchestrator,
//! reply with the assistant's final text.
//!
//! Only text DMs are handled in v1. Group chats, slash commands, photos,
//! and edits are ignored.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::ChatId;

use crate::binder::SessionBinder;
use crate::daemon::{DaemonClient, FireOutcome};
use crate::orchestrator::{handle_message, Fire, OrchestratorConfig, Replier};

#[async_trait]
impl Fire for DaemonClient {
    async fn fire_spawn(
        &self,
        preset: &str,
        project_id: Option<String>,
        prompt: String,
        tags: std::collections::BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        DaemonClient::fire_spawn(self, preset, project_id, prompt, tags, timeout).await
    }

    async fn fire_resume(
        &self,
        session_id: &str,
        prompt: String,
        tags: std::collections::BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        DaemonClient::fire_resume(self, session_id, prompt, tags, timeout).await
    }
}

pub struct TeloxideReplier {
    bot: Bot,
}

impl TeloxideReplier {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }
}

#[async_trait]
impl Replier for TeloxideReplier {
    async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
        self.bot.send_message(ChatId(chat_id), text).await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct BotDeps {
    pub cfg: Arc<OrchestratorConfig>,
    pub binder: Arc<SessionBinder>,
    pub daemon: Arc<DaemonClient>,
    pub replier: Arc<TeloxideReplier>,
    pub allowed_user_ids: Arc<HashSet<u64>>,
}

/// Spawn the bot and block until ctrl-C / hangup.
pub async fn run(bot: Bot, deps: BotDeps) -> Result<()> {
    tracing::info!("starting teloxide dispatcher");

    let handler =
        Update::filter_message().endpoint(|bot: Bot, msg: Message, deps: BotDeps| async move {
            if let Err(e) = on_message(&bot, &msg, &deps).await {
                tracing::warn!(?e, chat_id = msg.chat.id.0, "message handler failed");
            }
            respond(())
        });

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![deps])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn on_message(_bot: &Bot, msg: &Message, deps: &BotDeps) -> Result<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0;
    if !deps.allowed_user_ids.is_empty() && !deps.allowed_user_ids.contains(&user_id) {
        tracing::debug!(user_id, "rejecting non-allowlisted sender");
        return Ok(());
    }

    handle_message(
        deps.cfg.as_ref(),
        deps.binder.as_ref(),
        deps.daemon.as_ref(),
        deps.replier.as_ref(),
        msg.chat.id.0,
        text.to_string(),
    )
    .await
}
