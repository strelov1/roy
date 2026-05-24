//! Teloxide bot loop. Dispatches /cancel vs text messages, runs the streaming
//! pipeline on text, and signals the cancel registry on /cancel.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, ChatId, MessageId, ParseMode};

use crate::binder::SessionBinder;
use crate::cancel::CancelRegistry;
use crate::daemon::RealConnFactory;
use crate::draft_stream::DraftReplier;
use crate::orchestrator::{handle_message, OrchestratorConfig, Replier};
use crate::typing::TypingReplier;

pub struct TeloxideReplier {
    bot: Bot,
}

impl TeloxideReplier {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }
}

#[async_trait]
impl DraftReplier for TeloxideReplier {
    async fn send(&self, chat_id: i64, html: &str) -> Result<i32> {
        let msg = self
            .bot
            .send_message(ChatId(chat_id), html)
            .parse_mode(ParseMode::Html)
            .await?;
        Ok(msg.id.0)
    }

    async fn edit(&self, chat_id: i64, message_id: i32, html: &str) -> Result<()> {
        self.bot
            .edit_message_text(ChatId(chat_id), MessageId(message_id), html)
            .parse_mode(ParseMode::Html)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl TypingReplier for TeloxideReplier {
    async fn typing(&self, chat_id: i64) -> Result<()> {
        self.bot
            .send_chat_action(ChatId(chat_id), ChatAction::Typing)
            .await?;
        Ok(())
    }
}

impl Replier for TeloxideReplier {}

#[derive(Clone)]
pub struct BotDeps {
    pub cfg: Arc<OrchestratorConfig>,
    pub binder: Arc<SessionBinder>,
    pub conn_factory: Arc<RealConnFactory>,
    pub replier: Arc<TeloxideReplier>,
    pub cancel_registry: Arc<CancelRegistry>,
    pub allowed_user_ids: Arc<HashSet<u64>>,
}

pub async fn run(bot: Bot, deps: BotDeps) -> Result<()> {
    tracing::info!("starting teloxide dispatcher");

    let handler =
        Update::filter_message().endpoint(|_bot: Bot, msg: Message, deps: BotDeps| async move {
            if let Err(e) = on_message(&msg, &deps).await {
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

async fn on_message(msg: &Message, deps: &BotDeps) -> Result<()> {
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

    let chat_id = msg.chat.id.0;

    if is_cancel_command(text) {
        on_cancel(deps, chat_id).await
    } else {
        handle_message(
            deps.cfg.as_ref(),
            deps.binder.as_ref(),
            deps.cancel_registry.as_ref(),
            deps.conn_factory.as_ref(),
            &deps.replier,
            chat_id,
            text.to_string(),
        )
        .await
    }
}

fn is_cancel_command(text: &str) -> bool {
    let head = text.split_whitespace().next().unwrap_or("");
    head == "/cancel" || head.starts_with("/cancel@")
}

async fn on_cancel(deps: &BotDeps, chat_id: i64) -> Result<()> {
    let signaled = deps.cancel_registry.signal(chat_id).await;
    let reply = if signaled {
        "❎ cancelled"
    } else {
        "Нечего отменять — turn не запущен"
    };
    deps.replier.send(chat_id, reply).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_plain_cancel() {
        assert!(is_cancel_command("/cancel"));
        assert!(is_cancel_command("/cancel  "));
        assert!(is_cancel_command("/cancel reason ignored"));
    }

    #[test]
    fn detects_cancel_with_bot_suffix() {
        assert!(is_cancel_command("/cancel@my_bot"));
    }

    #[test]
    fn does_not_match_other_text() {
        assert!(!is_cancel_command("cancel"));
        assert!(!is_cancel_command("hello /cancel"));
        assert!(!is_cancel_command(""));
    }
}
