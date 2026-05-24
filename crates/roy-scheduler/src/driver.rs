//! Driver — the polling loop and per-fire invocation. Single-process
//! single-instance (PidLock added in Task 16).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use croner::Cron;
use sqlx::SqlitePool;

use crate::plan::{plan_tick, TickPlan};
use crate::store::triggers;
use crate::types::Trigger;

/// One polling tick: claim due rows in a short transaction, return the
/// rows the caller should dispatch through invoke_fire (OUTSIDE the txn).
pub async fn poll_tick(pool: &SqlitePool, batch_limit: i64) -> Result<Vec<Trigger>> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    let due = triggers::select_due(&mut tx, now, batch_limit).await?;
    let plan = plan_tick(&due, now, compute_next);

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
    Ok(plan.to_fire)
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
        assert_eq!(to_fire[0].id, t.id);

        // Trigger is gone after the tick.
        assert!(tstore::get_by_id(&pool, &t.id).await.unwrap().is_none());
    }
}
