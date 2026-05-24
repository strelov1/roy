//! Driver — the polling loop and per-fire invocation. Single-process
//! single-instance (PidLock added in Task 16).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use croner::Cron;
use sqlx::SqlitePool;

use crate::plan::plan_tick;
use crate::store::triggers;
use crate::types::Trigger;

/// A claimed trigger plus the `running` fire row already created for it.
/// `poll_tick` inserts the fire row inside the claim txn so the FK to
/// `triggers(id)` is still satisfied at INSERT time — without this,
/// oneshots (which `plan_tick` deletes in the same tick) would fail with
/// `FOREIGN KEY constraint failed` on the fire row.
#[derive(Debug, Clone)]
pub struct ClaimedFire {
    pub trigger: Trigger,
    pub fire_id: String,
}

/// One polling tick: claim due rows in a short transaction, return the
/// rows the caller should dispatch through invoke_fire (OUTSIDE the txn).
pub async fn poll_tick(pool: &SqlitePool, batch_limit: i64) -> Result<Vec<ClaimedFire>> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    let due = triggers::select_due(&mut tx, now, batch_limit).await?;
    let plan = plan_tick(&due, now, compute_next);

    // Insert the `running` fire row for each to_fire trigger BEFORE the
    // trigger delete fires the ON DELETE SET NULL — otherwise a oneshot's
    // delete runs before the fire INSERT and SQLite rejects the INSERT.
    let mut claimed: Vec<ClaimedFire> = Vec::with_capacity(plan.to_fire.len());
    for trig in &plan.to_fire {
        let fire_id = fires::insert_running_in_txn(
            &mut tx,
            fires::NewFire {
                agent_id: trig.agent_id.clone(),
                trigger_id: Some(trig.id.clone()),
            },
        )
        .await?;
        claimed.push(ClaimedFire {
            trigger: trig.clone(),
            fire_id,
        });
    }

    for id in &plan.to_delete {
        triggers::delete_in_txn(&mut tx, id).await?;
    }
    for op in &plan.to_advance {
        triggers::advance_next_fire(&mut tx, &op.id, op.next_fire_at, op.last_fire_at).await?;
    }
    for op in &plan.to_pause {
        triggers::pause(&mut tx, &op.id, &op.last_error).await?;
    }

    tx.commit().await?;
    Ok(claimed)
}

/// croner-backed `next firing` function used by plan_tick.
fn compute_next(expr: &str, tz: &str) -> Option<DateTime<Utc>> {
    let cron = Cron::new(expr).parse().ok()?;
    let tz: chrono_tz::Tz = tz.parse().ok()?;
    let now = Utc::now().with_timezone(&tz);
    cron.find_next_occurrence(&now, false)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::roy_client::{self, FireOutcome};
use crate::store::{agents, fires};
use crate::subscribers;
use crate::types::{Agent, FireStatus};

#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub db_path: PathBuf,
    pub socket_path: PathBuf,
    pub poll_interval: Duration,
    pub batch_limit: i64,
    pub max_fires: usize,
    pub fire_timeout: Duration,
}

impl Default for ServeOpts {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            socket_path: default_socket_path(),
            poll_interval: Duration::from_millis(1500),
            batch_limit: 50,
            max_fires: 8,
            fire_timeout: Duration::from_secs(600),
        }
    }
}

fn default_db_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SCHEDULER_DB") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-scheduler/state.db")
}

fn default_socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

/// Top-level entry. Opens the DB, runs the crash-recovery sweep, then
/// polls forever. Caller installs the PidLock (see src/main.rs Task 18).
pub async fn serve(opts: ServeOpts) -> Result<()> {
    let pool = crate::db::open(&opts.db_path).await?;

    let swept = fires::sweep_running_older_than(&pool, fires::default_sweep_cutoff()).await?;
    if swept > 0 {
        tracing::warn!(rows = swept, "swept stuck running fires on startup");
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(opts.max_fires));
    let pool = Arc::new(pool);
    let socket_path = Arc::new(opts.socket_path.clone());

    loop {
        match poll_tick(&pool, opts.batch_limit).await {
            Ok(to_fire) => {
                for ClaimedFire { trigger, fire_id } in to_fire {
                    let permit = match Arc::clone(&semaphore).acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let pool = Arc::clone(&pool);
                    let socket_path = Arc::clone(&socket_path);
                    let fire_timeout = opts.fire_timeout;
                    tokio::spawn(async move {
                        if let Err(e) =
                            invoke_fire(&pool, &socket_path, trigger, fire_id, fire_timeout).await
                        {
                            tracing::error!(error = %e, "invoke_fire failed");
                        }
                        drop(permit);
                    });
                }
            }
            Err(e) => tracing::error!(error = %e, "poll_tick failed"),
        }
        tokio::time::sleep(opts.poll_interval).await;
    }
}

/// Run a fire whose `running` row was already inserted upstream by
/// `poll_tick` (or `cmd_fire_now` for ad-hoc invocations). Fetches the
/// agent, invokes it, then writes the terminal update + dispatches
/// subscribers.
pub async fn invoke_fire(
    pool: &SqlitePool,
    socket_path: &std::path::Path,
    trigger: Trigger,
    fire_id: String,
    fire_timeout: Duration,
) -> Result<()> {
    let agent = agents::get_by_id(pool, &trigger.agent_id)
        .await?
        .with_context(|| format!("agent {} missing", trigger.agent_id))?;

    let mut tags = BTreeMap::new();
    tags.insert("roy-scheduler:agent_id".into(), agent.id.clone());
    tags.insert("roy-scheduler:trigger_id".into(), trigger.id.clone());
    tags.insert("roy-scheduler:fire_id".into(), fire_id.clone());
    tags.insert("roy-scheduler:kind".into(), "background_fire".into());

    let target = build_target(&agent);
    let outcome =
        roy_client::fire(socket_path, target, agent.task.clone(), tags, fire_timeout).await;

    let (terminal, success_ref, error_msg) = match outcome {
        Ok(FireOutcome::Done(s)) => (
            fires::TerminalUpdate {
                status: FireStatus::Ok,
                session_id: Some(s.session_id.clone()),
                seq_range: Some((s.seq_range.0 as i64, s.seq_range.1 as i64)),
                assistant_text: Some(s.assistant_text.clone()),
                cost_usd: s.cost_usd,
                stop_reason: Some(s.stop_reason.clone()),
                error_message: None,
            },
            Some(s),
            None,
        ),
        Ok(FireOutcome::Timeout {
            session_id,
            partial_seq_range,
        }) => (
            fires::TerminalUpdate {
                status: FireStatus::Timeout,
                session_id: Some(session_id),
                seq_range: Some((partial_seq_range.0 as i64, partial_seq_range.1 as i64)),
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some("fire timed out".into()),
            },
            None,
            Some("fire timed out".to_string()),
        ),
        Ok(FireOutcome::Error {
            session_id,
            code,
            message,
        }) => (
            fires::TerminalUpdate {
                status: FireStatus::Error,
                session_id,
                seq_range: None,
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some(format!("{code}: {message}")),
            },
            None,
            Some(format!("{code}: {message}")),
        ),
        Err(e) => (
            fires::TerminalUpdate {
                status: FireStatus::Error,
                session_id: None,
                seq_range: None,
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some(format!("roy_client: {e:#}")),
            },
            None,
            Some(format!("roy_client: {e:#}")),
        ),
    };

    fires::update_terminal(pool, &fire_id, terminal).await?;

    // If we used Spawn but the agent is persistent, capture the new session id.
    if agent.is_persistent() && agent.persistent_session_id.is_none() {
        if let Some(ref s) = success_ref {
            agents::update_persistent_session_id(pool, &agent.id, Some(&s.session_id)).await?;
        }
    }

    let fire = fires::get_by_id(pool, &fire_id).await?.expect("fire row");
    subscribers::dispatch(
        pool,
        socket_path,
        &fire,
        &agent.name,
        success_ref.as_ref(),
        error_msg.as_deref(),
    )
    .await?;

    Ok(())
}

fn build_target(agent: &Agent) -> roy::FireTarget {
    if agent.is_persistent() {
        if let Some(sid) = agent.persistent_session_id.as_ref() {
            return roy::FireTarget::Resume {
                session_id: sid.clone(),
            };
        }
    }
    roy::FireTarget::Spawn {
        preset: agent.preset.clone(),
        project_id: agent.project_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db,
        store::{agents, triggers as tstore},
    };
    use chrono::Duration as CDur;
    use tempfile::tempdir;

    #[tokio::test]
    async fn poll_tick_advances_cron_and_returns_to_fire() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
            },
        )
        .await
        .unwrap();
        let _trig = tstore::insert_cron(
            &pool,
            tstore::NewCronTrigger {
                agent_id: a.id,
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() - CDur::seconds(10),
            },
        )
        .await
        .unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert_eq!(to_fire.len(), 1);

        // Second tick: nothing (next_fire_at was advanced).
        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert!(to_fire.is_empty());
    }

    #[tokio::test]
    async fn poll_tick_pauses_bad_cron() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
            },
        )
        .await
        .unwrap();
        let t = tstore::insert_cron(
            &pool,
            tstore::NewCronTrigger {
                agent_id: a.id,
                cron_expr: "this-is-garbage".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() - CDur::seconds(10),
            },
        )
        .await
        .unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert!(to_fire.is_empty());

        let back = tstore::get_by_id(&pool, &t.id).await.unwrap().unwrap();
        assert!(back.is_paused());
        assert_eq!(back.last_error.as_deref(), Some("invalid cron"));
    }

    #[tokio::test]
    async fn poll_tick_deletes_oneshot_and_returns_it() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
            },
        )
        .await
        .unwrap();
        let agent_id = a.id.clone();
        let t = tstore::insert_oneshot(
            &pool,
            tstore::NewOneshotTrigger {
                agent_id: a.id,
                fire_at: Utc::now() - CDur::seconds(10),
            },
        )
        .await
        .unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert_eq!(to_fire.len(), 1);
        assert_eq!(to_fire[0].trigger.id, t.id);

        // Trigger is gone after the tick.
        assert!(tstore::get_by_id(&pool, &t.id).await.unwrap().is_none());

        // The fire row exists and its trigger_id was set NULL by the
        // ON DELETE SET NULL fk (the oneshot trigger was deleted in the
        // same claim txn that inserted the fire).
        let fire = fires::get_by_id(&pool, &to_fire[0].fire_id)
            .await
            .unwrap()
            .expect("fire row");
        assert_eq!(fire.status, "running");
        assert_eq!(fire.agent_id, agent_id);
        assert!(fire.trigger_id.is_none());
    }
}
