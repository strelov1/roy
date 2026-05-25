//! agents table CRUD.

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::types::Agent;

pub struct NewAgent {
    pub name: String,
    pub preset: String,
    /// `Some(id)` fires inside that roy-side project; `None` fires orphan.
    pub project_id: Option<String>,
    pub task: String,
    pub model: Option<String>,
    pub persistent: bool,
    pub notify_session: Option<String>,
}

pub async fn insert(pool: &SqlitePool, new: NewAgent) -> Result<Agent> {
    // `notify_session` is templated unescaped into the agent's prompt as a
    // `roy inject <id> "..."` instruction. Roy session ids are UUIDs (minted
    // by `SessionEngine::spawn`), so require the same shape here — that makes
    // a malformed value impossible to persist instead of needing per-callsite
    // quoting in the template.
    if let Some(sid) = new.notify_session.as_deref() {
        Uuid::parse_str(sid)
            .with_context(|| format!("notify_session must be a UUID (got: {sid:?})"))?;
    }
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let persistent_int: i64 = if new.persistent { 1 } else { 0 };

    sqlx::query(
        "INSERT INTO agents (id, name, preset, project_id, task, model, persistent, notify_session, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.name)
    .bind(&new.preset)
    .bind(&new.project_id)
    .bind(&new.task)
    .bind(&new.model)
    .bind(persistent_int)
    .bind(&new.notify_session)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    get_by_id(pool, &id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("agent missing after insert"))
}

pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Agent>> {
    let agent = sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(agent)
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<Agent>> {
    let agents = sqlx::query_as::<_, Agent>("SELECT * FROM agents ORDER BY created_at DESC")
        .fetch_all(pool)
        .await?;
    Ok(agents)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool> {
    let n = sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub async fn update_persistent_session_id(
    pool: &SqlitePool,
    agent_id: &str,
    session_id: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE agents SET persistent_session_id = ?, updated_at = ? WHERE id = ?")
        .bind(session_id)
        .bind(Utc::now())
        .bind(agent_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::tempdir;

    async fn fresh_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        (dir, pool)
    }

    fn sample() -> NewAgent {
        NewAgent {
            name: "daily-digest".into(),
            preset: "claude".into(),
            project_id: None,
            task: "summarize today".into(),
            model: None,
            persistent: false,
            notify_session: None,
        }
    }

    #[tokio::test]
    async fn insert_then_get_returns_same_agent() {
        let (_d, pool) = fresh_pool().await;
        let inserted = insert(&pool, sample()).await.unwrap();
        let fetched = get_by_id(&pool, &inserted.id).await.unwrap().unwrap();
        assert_eq!(inserted.id, fetched.id);
        assert_eq!(fetched.name, "daily-digest");
        assert_eq!(fetched.preset, "claude");
        assert!(!fetched.is_persistent());
        assert!(fetched.project_id.is_none());
    }

    #[tokio::test]
    async fn list_orders_newest_first() {
        let (_d, pool) = fresh_pool().await;
        let a1 = insert(&pool, sample()).await.unwrap();
        // ensure clock advances at least one tick
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut s2 = sample();
        s2.name = "second".into();
        let a2 = insert(&pool, s2).await.unwrap();

        let listed = list(&pool).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, a2.id, "newest first");
        assert_eq!(listed[1].id, a1.id);
    }

    #[tokio::test]
    async fn delete_removes_then_get_returns_none() {
        let (_d, pool) = fresh_pool().await;
        let a = insert(&pool, sample()).await.unwrap();
        assert!(delete(&pool, &a.id).await.unwrap());
        assert!(get_by_id(&pool, &a.id).await.unwrap().is_none());
        // second delete returns false (no row).
        assert!(!delete(&pool, &a.id).await.unwrap());
    }

    #[tokio::test]
    async fn notify_session_round_trips() {
        let (_d, pool) = fresh_pool().await;
        let sid = Uuid::new_v4().to_string();
        let mut n = sample();
        n.notify_session = Some(sid.clone());
        let a = insert(&pool, n).await.unwrap();
        let back = get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.notify_session.as_deref(), Some(sid.as_str()));
    }

    #[tokio::test]
    async fn notify_session_rejects_non_uuid() {
        let (_d, pool) = fresh_pool().await;
        let mut n = sample();
        n.notify_session = Some("not a uuid".into());
        let err = insert(&pool, n)
            .await
            .expect_err("non-UUID must be rejected");
        assert!(
            format!("{err:#}").contains("notify_session must be a UUID"),
            "unexpected error: {err:#}",
        );
    }

    #[tokio::test]
    async fn update_persistent_session_id_round_trips() {
        let (_d, pool) = fresh_pool().await;
        let a = insert(&pool, sample()).await.unwrap();
        update_persistent_session_id(&pool, &a.id, Some("roy-sid-1"))
            .await
            .unwrap();
        let back = get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.persistent_session_id.as_deref(), Some("roy-sid-1"));

        update_persistent_session_id(&pool, &a.id, None)
            .await
            .unwrap();
        let back = get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.persistent_session_id, None);
    }
}
