//! Subscriber dispatcher. Called by `driver::invoke_fire` after a Fire
//! completes. Loads enabled subscribers (agent OR trigger scope), iterates
//! in `order_index ASC, created_at ASC`, executes per-kind, writes a
//! `fire_subscriber_runs` row per attempt. At-most-once per fire — no
//! retry in v1.

use std::path::Path;

use anyhow::Result;
use sqlx::SqlitePool;

use crate::roy_client::FireSuccess;
use crate::store::subscribers as sub_store;
use crate::types::{Fire, Subscriber, SubscriberKind};

pub mod inject_parent;
pub mod notify_native;
pub mod webhook;

pub async fn dispatch(
    pool: &SqlitePool,
    socket_path: &Path,
    fire: &Fire,
    agent_name: &str,
    success: Option<&FireSuccess>,
    error_message: Option<&str>,
) -> Result<()> {
    let subs = sub_store::load_for_fire(pool, &fire.agent_id, fire.trigger_id.as_deref()).await?;

    for sub in subs {
        let kind = match SubscriberKind::parse(&sub.kind) {
            Some(k) => k,
            None => {
                write_run(
                    pool,
                    &fire.id,
                    &sub,
                    "error",
                    Some(format!("unknown kind: {}", sub.kind)),
                    None,
                )
                .await?;
                continue;
            }
        };

        let (status, error, snippet) = match kind {
            SubscriberKind::InjectParent => match success {
                Some(s) => {
                    let out = inject_parent::execute(socket_path, &sub.config, s).await;
                    (out.status, out.error_message, None)
                }
                None => (
                    "skipped",
                    Some("inject_parent skipped (fire did not succeed)".into()),
                    None,
                ),
            },
            SubscriberKind::Webhook => {
                let ctx = webhook::build_context(fire, agent_name, success, error_message);
                let out = webhook::execute(&sub.config, &ctx).await;
                (out.status, out.error_message, out.response_snippet)
            }
            SubscriberKind::NotifyNative => match success {
                Some(s) => {
                    let out = notify_native::execute(&sub.config, agent_name, s).await;
                    (out.status, out.error_message, None)
                }
                None => (
                    "skipped",
                    Some("notify_native skipped (fire did not succeed)".into()),
                    None,
                ),
            },
        };

        write_run(pool, &fire.id, &sub, status, error, snippet).await?;
    }

    Ok(())
}

async fn write_run(
    pool: &SqlitePool,
    fire_id: &str,
    sub: &Subscriber,
    status: &str,
    error_message: Option<String>,
    response_snippet: Option<String>,
) -> Result<()> {
    sub_store::insert_run(
        pool,
        sub_store::NewSubscriberRun {
            fire_id: fire_id.into(),
            subscriber_id: sub.id.clone(),
            status: match status {
                "ok" => "ok",
                "skipped" => "skipped",
                _ => "error",
            },
            error_message,
            response_snippet,
        },
    )
    .await?;
    Ok(())
}
