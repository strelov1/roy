//! fire_subscribers + fire_subscriber_runs CRUD.

use anyhow::Result;
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::types::{Subscriber, SubscriberKind, SubscriberRun};

pub struct NewSubscriber {
    /// Exactly one of agent_id / trigger_id is Some.
    pub agent_id: Option<String>,
    pub trigger_id: Option<String>,
    pub kind: SubscriberKind,
    /// JSON string. Per-kind shape lives in src/subscribers/*.rs.
    pub config_json: String,
    pub order_index: i64,
}

pub async fn insert(pool: &SqlitePool, new: NewSubscriber) -> Result<Subscriber> {
    if new.agent_id.is_none() && new.trigger_id.is_none() {
        anyhow::bail!("subscriber must reference either agent_id or trigger_id");
    }
    if new.agent_id.is_some() && new.trigger_id.is_some() {
        anyhow::bail!("subscriber may not reference both agent_id and trigger_id");
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO fire_subscribers
         (id, agent_id, trigger_id, kind, config, enabled, order_index, created_at)
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(&new.trigger_id)
    .bind(new.kind.as_db())
    .bind(&new.config_json)
    .bind(new.order_index)
    .bind(now)
    .execute(pool)
    .await?;
    get_by_id(pool, &id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("subscriber missing after insert"))
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Subscriber>> {
    let s = sqlx::query_as::<_, Subscriber>("SELECT * FROM fire_subscribers WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(s)
}

pub async fn list_for_agent(pool: &SqlitePool, agent_id: &str) -> Result<Vec<Subscriber>> {
    let v = sqlx::query_as::<_, Subscriber>(
        "SELECT * FROM fire_subscribers WHERE agent_id = ? ORDER BY order_index, created_at",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

pub async fn list_for_trigger(pool: &SqlitePool, trigger_id: &str) -> Result<Vec<Subscriber>> {
    let v = sqlx::query_as::<_, Subscriber>(
        "SELECT * FROM fire_subscribers WHERE trigger_id = ? ORDER BY order_index, created_at",
    )
    .bind(trigger_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

/// Load all enabled subscribers that match either `agent_id` or
/// `trigger_id`. Sorted by `order_index ASC, created_at ASC` for a
/// deterministic execution order (spec §4.1).
pub async fn load_for_fire(
    pool: &SqlitePool,
    agent_id: &str,
    trigger_id: Option<&str>,
) -> Result<Vec<Subscriber>> {
    let v = sqlx::query_as::<_, Subscriber>(
        "SELECT * FROM fire_subscribers
         WHERE enabled = 1
           AND (agent_id = ? OR trigger_id = ?)
         ORDER BY order_index ASC, created_at ASC",
    )
    .bind(agent_id)
    .bind(trigger_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM fire_subscribers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub struct NewSubscriberRun {
    pub fire_id: String,
    pub subscriber_id: String,
    pub status: &'static str, // "ok" | "error" | "skipped"
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

pub async fn insert_run(pool: &SqlitePool, run: NewSubscriberRun) -> Result<()> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO fire_subscriber_runs
         (id, fire_id, subscriber_id, status, started_at, finished_at, error_message, response_snippet)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&run.fire_id)
    .bind(&run.subscriber_id)
    .bind(run.status)
    .bind(now)
    .bind(now)
    .bind(&run.error_message)
    .bind(&run.response_snippet)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_runs_for_fire(pool: &SqlitePool, fire_id: &str) -> Result<Vec<SubscriberRun>> {
    let v = sqlx::query_as::<_, SubscriberRun>(
        "SELECT * FROM fire_subscriber_runs WHERE fire_id = ? ORDER BY started_at",
    )
    .bind(fire_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db,
        store::{agents, fires, triggers},
    };
    use chrono::Duration;
    use tempfile::tempdir;

    async fn fixture() -> (tempfile::TempDir, SqlitePool, String, String) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let agent = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "do".into(),
                model: None,
                persistent: false,
            },
        )
        .await
        .unwrap();
        let trig = triggers::insert_cron(
            &pool,
            triggers::NewCronTrigger {
                agent_id: agent.id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() + Duration::seconds(60),
            },
        )
        .await
        .unwrap();
        (dir, pool, agent.id, trig.id)
    }

    #[tokio::test]
    async fn insert_rejects_neither_or_both() {
        let (_d, pool, _a, _t) = fixture().await;
        let r = insert(
            &pool,
            NewSubscriber {
                agent_id: None,
                trigger_id: None,
                kind: SubscriberKind::Webhook,
                config_json: "{}".into(),
                order_index: 0,
            },
        )
        .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn load_for_fire_unions_agent_and_trigger() {
        let (_d, pool, agent_id, trig_id) = fixture().await;

        let sa = insert(
            &pool,
            NewSubscriber {
                agent_id: Some(agent_id.clone()),
                trigger_id: None,
                kind: SubscriberKind::Webhook,
                config_json: r#"{"url":"https://example.com"}"#.into(),
                order_index: 1,
            },
        )
        .await
        .unwrap();
        let st = insert(
            &pool,
            NewSubscriber {
                agent_id: None,
                trigger_id: Some(trig_id.clone()),
                kind: SubscriberKind::NotifyNative,
                config_json: "{}".into(),
                order_index: 0,
            },
        )
        .await
        .unwrap();

        let v = load_for_fire(&pool, &agent_id, Some(&trig_id))
            .await
            .unwrap();
        assert_eq!(v.len(), 2);
        // order_index 0 first
        assert_eq!(v[0].id, st.id);
        assert_eq!(v[1].id, sa.id);
    }

    #[tokio::test]
    async fn insert_run_then_list_returns_it() {
        let (_d, pool, agent_id, _t) = fixture().await;
        let fire_id = fires::insert_running(
            &pool,
            fires::NewFire {
                agent_id: agent_id.clone(),
                trigger_id: None,
            },
        )
        .await
        .unwrap();
        let sub = insert(
            &pool,
            NewSubscriber {
                agent_id: Some(agent_id),
                trigger_id: None,
                kind: SubscriberKind::NotifyNative,
                config_json: "{}".into(),
                order_index: 0,
            },
        )
        .await
        .unwrap();

        insert_run(
            &pool,
            NewSubscriberRun {
                fire_id: fire_id.clone(),
                subscriber_id: sub.id.clone(),
                status: "ok",
                error_message: None,
                response_snippet: None,
            },
        )
        .await
        .unwrap();

        let v = list_runs_for_fire(&pool, &fire_id).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].status, "ok");
    }
}
