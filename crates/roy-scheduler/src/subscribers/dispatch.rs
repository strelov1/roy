//! Subscriber dispatcher. Loads enabled subscribers for a fire, builds each
//! via the registry, runs them in order, and writes one fire_subscriber_runs
//! row per attempt. At-most-once per fire — no retry in v1.

use std::path::Path;

use anyhow::Result;
use sqlx::SqlitePool;

use super::registry::registry;
use super::Outcome;
use crate::roy_client::FireSuccess;
use crate::store::subscribers as sub_store;
use crate::types::{Fire, Subscriber as SubscriberRow, SubscriberKind};

pub async fn dispatch(
    pool: &SqlitePool,
    socket_path: &Path,
    fire: &Fire,
    agent_name: &str,
    success: Option<&FireSuccess>,
    error_message: Option<&str>,
) -> Result<()> {
    let subs = sub_store::load_for_fire(pool, &fire.agent_id, fire.trigger_id.as_deref()).await?;

    let ctx = super::FireCtx {
        socket_path,
        fire,
        agent_name,
        success,
        error_message,
    };

    for sub_row in subs {
        let outcome = run_one(&sub_row, &ctx).await;
        write_run(pool, &fire.id, &sub_row, outcome).await?;
    }

    Ok(())
}

async fn run_one(sub_row: &SubscriberRow, ctx: &super::FireCtx<'_>) -> Outcome {
    let Some(kind) = SubscriberKind::parse(&sub_row.kind) else {
        return Outcome::error(format!("unknown kind: {}", sub_row.kind));
    };
    let ctor = registry()
        .get(&kind)
        .expect("registry missing ctor for known SubscriberKind — registry::all_kinds_registered should catch this");
    match ctor(&sub_row.config) {
        Ok(sub) => sub.run(ctx).await,
        Err(e) => Outcome::error(format!("config parse: {e:#}")),
    }
}

async fn write_run(
    pool: &SqlitePool,
    fire_id: &str,
    sub: &SubscriberRow,
    outcome: Outcome,
) -> Result<()> {
    sub_store::insert_run(
        pool,
        sub_store::NewSubscriberRun {
            fire_id: fire_id.into(),
            subscriber_id: sub.id.clone(),
            status: outcome.status.as_db(),
            error_message: outcome.error_message,
            response_snippet: outcome.response_snippet,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::store::{agents, subscribers as sub_store};
    use tempfile::tempdir;

    #[tokio::test]
    async fn unknown_kind_writes_error_run() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                harness: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        // Insert a subscriber row with 'chain_agent' — still in the DB
        // schema's CHECK constraint (legacy value) but absent from
        // SubscriberKind::parse, so the dispatcher treats it as unknown.
        let sub_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO fire_subscribers (id, agent_id, kind, config, enabled, order_index, created_at) \
             VALUES (?, ?, 'chain_agent', '{}', 1, 0, datetime('now'))",
        )
        .bind(&sub_id)
        .bind(&a.id)
        .execute(&pool)
        .await
        .unwrap();

        // Fake a Fire row.
        let fire_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO fires (id, agent_id, status, started_at) \
             VALUES (?, ?, 'ok', datetime('now'))",
        )
        .bind(&fire_id)
        .bind(&a.id)
        .execute(&pool)
        .await
        .unwrap();
        let fire = crate::store::fires::get_by_id(&pool, &fire_id)
            .await
            .unwrap()
            .unwrap();

        dispatch(
            &pool,
            std::path::Path::new("/unused"),
            &fire,
            "agent",
            None,
            None,
        )
        .await
        .unwrap();

        let runs = sub_store::list_runs_for_fire(&pool, &fire.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "error");
        assert!(runs[0]
            .error_message
            .as_deref()
            .unwrap()
            .contains("unknown kind"));
    }
}
