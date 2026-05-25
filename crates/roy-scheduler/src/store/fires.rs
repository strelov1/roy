//! fires table CRUD + crash-recovery sweep used on startup.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::types::{Fire, FireStatus};

pub struct NewFire {
    pub agent_id: String,
    pub trigger_id: Option<String>,
}

/// Insert a `running` fire row. Returns the new id.
pub async fn insert_running(pool: &SqlitePool, new: NewFire) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO fires (id, agent_id, trigger_id, status, started_at)
         VALUES (?, ?, ?, 'running', ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(&new.trigger_id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Transaction variant — same as `insert_running` but participates in the
/// caller's claim txn. Used by `poll_tick` so a fire row exists before the
/// oneshot trigger row gets `ON DELETE SET NULL`d, avoiding an FK violation
/// on INSERT.
pub async fn insert_running_in_txn(
    tx: &mut Transaction<'_, Sqlite>,
    new: NewFire,
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO fires (id, agent_id, trigger_id, status, started_at)
         VALUES (?, ?, ?, 'running', ?)",
    )
    .bind(&id)
    .bind(&new.agent_id)
    .bind(&new.trigger_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

pub struct TerminalUpdate {
    pub status: FireStatus,
    pub session_id: Option<String>,
    pub seq_range: Option<(i64, i64)>,
    pub assistant_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
}

pub async fn update_terminal(pool: &SqlitePool, id: &str, t: TerminalUpdate) -> Result<()> {
    let (seq_start, seq_end) = match t.seq_range {
        Some((s, e)) => (Some(s), Some(e)),
        None => (None, None),
    };
    sqlx::query(
        "UPDATE fires SET
            status = ?,
            session_id = COALESCE(?, session_id),
            transcript_seq_range_start = ?,
            transcript_seq_range_end = ?,
            assistant_text = ?,
            cost_usd = ?,
            stop_reason = ?,
            error_message = ?,
            finished_at = ?
         WHERE id = ?",
    )
    .bind(t.status.as_db())
    .bind(&t.session_id)
    .bind(seq_start)
    .bind(seq_end)
    .bind(&t.assistant_text)
    .bind(t.cost_usd)
    .bind(&t.stop_reason)
    .bind(&t.error_message)
    .bind(Utc::now())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sweep stuck `running` fires that started more than `older_than` ago.
/// Used on driver startup to mark crashed fires as errors.
/// Returns count of rows touched.
pub async fn sweep_running_older_than(pool: &SqlitePool, cutoff: DateTime<Utc>) -> Result<u64> {
    let n = sqlx::query(
        "UPDATE fires SET status = 'error',
                          error_message = 'scheduler crashed',
                          finished_at = ?
         WHERE status = 'running' AND started_at < ?",
    )
    .bind(Utc::now())
    .bind(cutoff)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n)
}

pub fn default_sweep_cutoff() -> DateTime<Utc> {
    Utc::now() - Duration::minutes(15)
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Fire>> {
    let f = sqlx::query_as::<_, Fire>("SELECT * FROM fires WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(f)
}

pub async fn list_for_agent(pool: &SqlitePool, agent_id: &str, limit: i64) -> Result<Vec<Fire>> {
    let v = sqlx::query_as::<_, Fire>(
        "SELECT * FROM fires WHERE agent_id = ? ORDER BY started_at DESC LIMIT ?",
    )
    .bind(agent_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(v)
}

pub async fn list_recent(pool: &SqlitePool, limit: i64) -> Result<Vec<Fire>> {
    let v = sqlx::query_as::<_, Fire>("SELECT * FROM fires ORDER BY started_at DESC LIMIT ?")
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, store::agents};
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
    async fn insert_running_then_terminal_updates_status() {
        let (_d, pool, agent_id) = fixture().await;
        let fire_id = insert_running(
            &pool,
            NewFire {
                agent_id,
                trigger_id: None,
            },
        )
        .await
        .unwrap();

        update_terminal(
            &pool,
            &fire_id,
            TerminalUpdate {
                status: FireStatus::Ok,
                session_id: Some("roy-sid".into()),
                seq_range: Some((5, 12)),
                assistant_text: Some("hello".into()),
                cost_usd: Some(0.001),
                stop_reason: Some("end_turn".into()),
                error_message: None,
            },
        )
        .await
        .unwrap();

        let f = get_by_id(&pool, &fire_id).await.unwrap().unwrap();
        assert_eq!(f.status, "ok");
        assert_eq!(f.session_id.as_deref(), Some("roy-sid"));
        assert_eq!(f.transcript_seq_range_start, Some(5));
        assert_eq!(f.transcript_seq_range_end, Some(12));
        assert_eq!(f.assistant_text.as_deref(), Some("hello"));
        assert!(f.finished_at.is_some());
    }

    #[tokio::test]
    async fn sweep_marks_old_running_as_error() {
        let (_d, pool, agent_id) = fixture().await;
        let fire_id = insert_running(
            &pool,
            NewFire {
                agent_id: agent_id.clone(),
                trigger_id: None,
            },
        )
        .await
        .unwrap();

        // Force started_at into the past so the sweep claims it.
        let past = Utc::now() - chrono::Duration::hours(1);
        sqlx::query("UPDATE fires SET started_at = ? WHERE id = ?")
            .bind(past)
            .bind(&fire_id)
            .execute(&pool)
            .await
            .unwrap();

        let n = sweep_running_older_than(&pool, default_sweep_cutoff())
            .await
            .unwrap();
        assert_eq!(n, 1);

        let f = get_by_id(&pool, &fire_id).await.unwrap().unwrap();
        assert_eq!(f.status, "error");
        assert_eq!(f.error_message.as_deref(), Some("scheduler crashed"));

        // A fresh running fire should NOT be swept.
        let f2 = insert_running(
            &pool,
            NewFire {
                agent_id,
                trigger_id: None,
            },
        )
        .await
        .unwrap();
        let n2 = sweep_running_older_than(&pool, default_sweep_cutoff())
            .await
            .unwrap();
        assert_eq!(n2, 0);
        let still_running = get_by_id(&pool, &f2).await.unwrap().unwrap();
        assert_eq!(still_running.status, "running");
    }

    #[tokio::test]
    async fn list_for_agent_newest_first_with_limit() {
        let (_d, pool, agent_id) = fixture().await;
        for _ in 0..5 {
            insert_running(
                &pool,
                NewFire {
                    agent_id: agent_id.clone(),
                    trigger_id: None,
                },
            )
            .await
            .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let v = list_for_agent(&pool, &agent_id, 3).await.unwrap();
        assert_eq!(v.len(), 3);
        for w in v.windows(2) {
            assert!(w[0].started_at >= w[1].started_at);
        }
    }
}
