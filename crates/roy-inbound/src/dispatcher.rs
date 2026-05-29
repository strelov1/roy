//! The single bus consumer. Receives InboundEvents, routes, resolves
//! session, fires, writes bindings.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::bus::{BusReceiver, EventRef, InboundEvent};
use crate::daemon_client::{fire_with_hook, OutcomeKind};
use crate::reply::{FireOutcome, ReplyHookRegistry};
use crate::router::Router;
use crate::session::SessionResolver;
use crate::store::bindings::BindingStore;

pub struct InboundDispatcher {
    pub bus: BusReceiver,
    pub router: Arc<dyn Router>,
    pub resolver: SessionResolver,
    pub bindings: Arc<BindingStore>,
    pub hooks: Arc<ReplyHookRegistry>,
    pub socket_path: PathBuf,
}

impl InboundDispatcher {
    pub async fn run(mut self, cancel: CancellationToken) -> Result<()> {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                next = self.bus.recv() => {
                    let Some(event) = next else { return Ok(()); };
                    if let Err(e) = self.handle_one(event).await {
                        tracing::error!(error = ?e, "dispatcher handle_one error (continuing)");
                    }
                }
            }
        }
    }

    async fn handle_one(&self, event: InboundEvent) -> Result<()> {
        let ev_ref = EventRef::from(&event);
        let kind = event.source_kind.clone();

        // Route.
        let spec = match self.router.route(&event).await {
            Some(s) => s,
            None => {
                let hook = self
                    .hooks
                    .make(&kind, &ev_ref)
                    .ok_or_else(|| anyhow::anyhow!("no reply hook for kind '{kind}'"))?;
                hook.on_finish(FireOutcome::RouteRejected, event.reply)
                    .await?;
                return Ok(());
            }
        };

        // Resolve session.
        let (target, pending) = self
            .resolver
            .resolve(
                &event.source_id,
                &event.sender_id,
                &spec.agent_id,
                spec.session_strategy,
                spec.harness.as_deref(),
                spec.system_prompt.as_deref(),
            )
            .await?;

        // Build the hook for this fire.
        let hook = self
            .hooks
            .make(&kind, &ev_ref)
            .ok_or_else(|| anyhow::anyhow!("no reply hook for kind '{kind}'"))?;

        // Fire.
        let was_resume = !matches!(target, roy_protocol::FireTarget::Spawn { .. });
        let result = fire_with_hook(
            &self.socket_path,
            target,
            spec.prompt,
            spec.tags,
            Duration::from_secs(spec.fire_timeout_secs),
            hook,
            event.reply,
        )
        .await?;

        // Binding writes: only on success when we deliberately Spawned a
        // sticky/persistent session.
        match &result.outcome_kind {
            OutcomeKind::Ok => {
                if let (Some(pb), Some(sid)) = (pending, result.session_id.as_ref()) {
                    self.bindings
                        .upsert(
                            &pb.source_id,
                            &pb.sender_id,
                            &pb.agent_id,
                            pb.strategy_db_label,
                            sid,
                        )
                        .await?;
                } else if was_resume {
                    // Touch the existing binding so last_active_at moves forward.
                    if let Some(b) = self
                        .bindings
                        .lookup(&event.source_id, &event.sender_id)
                        .await?
                    {
                        self.bindings.touch(&b.id).await?;
                    } else if let Some(b) = self.bindings.lookup(&event.source_id, "*").await? {
                        self.bindings.touch(&b.id).await?;
                    }
                }
            }
            // Stale-binding cleanup: a NoSession on a Resume means the
            // persisted session id is dead. Delete the row so the next event
            // from the same sender re-Spawns instead of hitting the same
            // dead id forever. The current event's reply was already
            // delivered as DaemonError by the hook — silent in-fire retry
            // is documented as deferred (spec Open Q §6).
            OutcomeKind::DaemonError(code) if code == "no_session" && was_resume => {
                if let Some(b) = self
                    .bindings
                    .lookup(&event.source_id, &event.sender_id)
                    .await?
                {
                    self.bindings.delete(&b.id).await?;
                } else if let Some(b) = self.bindings.lookup(&event.source_id, "*").await? {
                    self.bindings.delete(&b.id).await?;
                }
            }
            _ => {}
        }

        Ok(())
    }
}
