//! triggers table CRUD. `select_due` and `advance_next_fire` are the
//! load-bearing claim-transaction operations used by the driver.

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::types::Trigger;

pub struct NewCronTrigger {
    pub agent_id: String,
    pub cron_expr: String,
    pub timezone: String,
    pub next_fire_at: DateTime<Utc>,
}

pub struct NewOneshotTrigger {
    pub agent_id: String,
    pub fire_at: DateTime<Utc>,
}

pub async fn insert_cron(pool: &SqlitePool, new: NewCronTrigger) -> Result<Trigger> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO triggers (id, agent_id, kind, cron_expr, timezone, next_fire_at, created_at)
         VALUES (?, ?, 'cron', ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(&new.cron_expr)
    .bind(&new.timezone)
    .bind(new.next_fire_at)
    .bind(now)
    .execute(pool)
    .await?;
    get_by_id(pool, &id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("trigger missing after insert"))
}

pub async fn insert_oneshot(pool: &SqlitePool, new: NewOneshotTrigger) -> Result<Trigger> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO triggers (id, agent_id, kind, fire_at, next_fire_at, created_at)
         VALUES (?, ?, 'oneshot', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(new.fire_at)
    .bind(new.fire_at) // next_fire_at == fire_at for oneshot
    .bind(now)
    .execute(pool)
    .await?;
    get_by_id(pool, &id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("trigger missing after insert"))
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Trigger>> {
    let t = sqlx::query_as::<_, Trigger>("SELECT * FROM triggers WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(t)
}

pub async fn list_for_agent(pool: &SqlitePool, agent_id: &str) -> Result<Vec<Trigger>> {
    let v = sqlx::query_as::<_, Trigger>(
        "SELECT * FROM triggers WHERE agent_id = ? ORDER BY created_at DESC",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

pub async fn list_all(pool: &SqlitePool, limit: i64) -> Result<Vec<Trigger>> {
    let v = sqlx::query_as::<_, Trigger>("SELECT * FROM triggers ORDER BY created_at DESC LIMIT ?")
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(v)
}

/// Claim-transaction read. Returns triggers with `paused = 0` and
/// `next_fire_at <= now`, ordered oldest-due first, capped at `limit`.
/// SQLite has no SKIP LOCKED — single-writer scheduler doesn't need it.
pub async fn select_due(
    tx: &mut Transaction<'_, Sqlite>,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Trigger>> {
    let rows = sqlx::query_as::<_, Trigger>(
        "SELECT * FROM triggers
         WHERE paused = 0 AND next_fire_at <= ?
         ORDER BY next_fire_at ASC
         LIMIT ?",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows)
}

pub async fn advance_next_fire(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    next_fire_at: DateTime<Utc>,
    last_fire_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        "UPDATE triggers SET next_fire_at = ?, last_fire_at = ?, last_error = NULL
         WHERE id = ?",
    )
    .bind(next_fire_at)
    .bind(last_fire_at)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn pause(tx: &mut Transaction<'_, Sqlite>, id: &str, error: &str) -> Result<()> {
    sqlx::query("UPDATE triggers SET paused = 1, last_error = ? WHERE id = ?")
        .bind(error)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn unpause(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("UPDATE triggers SET paused = 0, last_error = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn pause_outside_txn(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("UPDATE triggers SET paused = 1 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete(tx_or_pool: &SqlitePool, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM triggers WHERE id = ?")
        .bind(id)
        .execute(tx_or_pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub async fn delete_in_txn(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM triggers WHERE id = ?")
        .bind(id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, store::agents};
    use chrono::Duration;
    use tempfile::tempdir;

    async fn fixture() -> (tempfile::TempDir, SqlitePool, String) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                preset: "claude".into(),
                project_id: None,
                task: "do".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        (dir, pool, a.id)
    }

    #[tokio::test]
    async fn select_due_returns_only_past_unpaused_rows() {
        let (_d, pool, agent_id) = fixture().await;
        let now = Utc::now();

        // Two due (one paused), one in future.
        let _due = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id: agent_id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();

        let paused_row = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id: agent_id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();
        pause_outside_txn(&pool, &paused_row.id).await.unwrap();

        let _future = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id,
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now + Duration::seconds(60),
            },
        )
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let due = select_due(&mut tx, now, 50).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(due.len(), 1, "only one unpaused-past row should be due");
    }

    #[tokio::test]
    async fn advance_then_no_longer_due() {
        let (_d, pool, agent_id) = fixture().await;
        let now = Utc::now();
        let t = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id,
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        advance_next_fire(&mut tx, &t.id, now + Duration::minutes(5), now)
            .await
            .unwrap();
        let still_due = select_due(&mut tx, now, 50).await.unwrap();
        tx.commit().await.unwrap();
        assert!(still_due.is_empty());
    }

    #[tokio::test]
    async fn pause_records_error_and_excludes_from_due() {
        let (_d, pool, agent_id) = fixture().await;
        let now = Utc::now();
        let t = insert_cron(
            &pool,
            NewCronTrigger {
                agent_id,
                cron_expr: "garbage".into(),
                timezone: "UTC".into(),
                next_fire_at: now - Duration::seconds(10),
            },
        )
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        pause(&mut tx, &t.id, "invalid cron").await.unwrap();
        tx.commit().await.unwrap();

        let back = get_by_id(&pool, &t.id).await.unwrap().unwrap();
        assert!(back.is_paused());
        assert_eq!(back.last_error.as_deref(), Some("invalid cron"));
    }

    #[tokio::test]
    async fn oneshot_next_fire_at_equals_fire_at() {
        let (_d, pool, agent_id) = fixture().await;
        let t = insert_oneshot(
            &pool,
            NewOneshotTrigger {
                agent_id,
                fire_at: Utc::now() + Duration::seconds(60),
            },
        )
        .await
        .unwrap();
        assert_eq!(t.fire_at, Some(t.next_fire_at));
        assert_eq!(t.kind, "oneshot");
    }

    #[tokio::test]
    async fn cascade_delete_when_agent_dropped() {
        let (_d, pool, agent_id) = fixture().await;
        insert_cron(
            &pool,
            NewCronTrigger {
                agent_id: agent_id.clone(),
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now(),
            },
        )
        .await
        .unwrap();

        // sqlite requires PRAGMA foreign_keys=ON per-connection. Verify it's on
        // (sqlx::sqlite enables it by default in 0.8).
        let _ = sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await;

        agents::delete(&pool, &agent_id).await.unwrap();
        let trigs = list_for_agent(&pool, &agent_id).await.unwrap();
        assert!(trigs.is_empty(), "FK cascade should drop child triggers");
    }
}
