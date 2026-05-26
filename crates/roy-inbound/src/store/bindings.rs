//! CRUD on the `bindings` table. The dispatcher calls `lookup`, then on a
//! fresh Spawn calls `upsert` with the daemon-issued session id. `touch`
//! refreshes `last_active_at` after a successful Resume.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, PartialEq)]
pub struct Binding {
    pub id: String,
    pub source_id: String,
    pub sender_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub strategy: String,
    pub created_at: DateTime<Utc>,
    pub last_active_at: DateTime<Utc>,
}

pub struct BindingStore {
    pool: SqlitePool,
}

impl BindingStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    #[cfg(test)]
    pub fn pool_for_test(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn lookup(&self, source_id: &str, sender_id: &str) -> Result<Option<Binding>> {
        let row: Option<Binding> = sqlx::query_as(
            "SELECT id, source_id, sender_id, session_id, agent_id, strategy, \
                    created_at, last_active_at \
             FROM bindings WHERE source_id = ?1 AND sender_id = ?2",
        )
        .bind(source_id)
        .bind(sender_id)
        .fetch_optional(&self.pool)
        .await
        .context("lookup binding")?;
        Ok(row)
    }

    pub async fn upsert(
        &self,
        source_id: &str,
        sender_id: &str,
        agent_id: &str,
        strategy: &str,
        session_id: &str,
    ) -> Result<Binding> {
        let now = Utc::now();
        let new_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO bindings (id, source_id, sender_id, session_id, agent_id, strategy, \
                                   created_at, last_active_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) \
             ON CONFLICT(source_id, sender_id) DO UPDATE SET \
                session_id = excluded.session_id, \
                agent_id = excluded.agent_id, \
                strategy = excluded.strategy, \
                last_active_at = excluded.last_active_at",
        )
        .bind(&new_id)
        .bind(source_id)
        .bind(sender_id)
        .bind(session_id)
        .bind(agent_id)
        .bind(strategy)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("upsert binding")?;
        self.lookup(source_id, sender_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("binding vanished after upsert"))
    }

    pub async fn touch(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE bindings SET last_active_at = ?1 WHERE id = ?2")
            .bind(Utc::now())
            .bind(id)
            .execute(&self.pool)
            .await
            .context("touch binding")?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM bindings WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete binding")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db;
    use tempfile::tempdir;

    async fn store() -> (tempfile::TempDir, BindingStore) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("s.db")).await.unwrap();
        (dir, BindingStore::new(pool))
    }

    #[tokio::test]
    async fn lookup_miss_returns_none() {
        let (_d, s) = store().await;
        let b = s.lookup("src", "alice").await.unwrap();
        assert!(b.is_none());
    }

    #[tokio::test]
    async fn upsert_then_lookup_returns_row() {
        let (_d, s) = store().await;
        let b = s
            .upsert("src", "alice", "agent-1", "per_sender_sticky", "sid-1")
            .await
            .unwrap();
        assert_eq!(b.session_id, "sid-1");
        let b2 = s.lookup("src", "alice").await.unwrap().unwrap();
        assert_eq!(b2.id, b.id);
    }

    #[tokio::test]
    async fn upsert_overwrites_existing() {
        let (_d, s) = store().await;
        let first = s
            .upsert("src", "alice", "agent-1", "per_sender_sticky", "old")
            .await
            .unwrap();
        let second = s
            .upsert("src", "alice", "agent-1", "per_sender_sticky", "new")
            .await
            .unwrap();
        assert_eq!(first.id, second.id, "upsert keeps same row id");
        assert_eq!(second.session_id, "new");
    }

    #[tokio::test]
    async fn touch_updates_last_active() {
        let (_d, s) = store().await;
        let b = s
            .upsert("src", "alice", "a", "per_sender_sticky", "sid")
            .await
            .unwrap();
        let before = b.last_active_at;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        s.touch(&b.id).await.unwrap();
        let after = s.lookup("src", "alice").await.unwrap().unwrap();
        assert!(after.last_active_at > before);
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let (_d, s) = store().await;
        let b = s
            .upsert("src", "alice", "a", "per_sender_sticky", "sid")
            .await
            .unwrap();
        s.delete(&b.id).await.unwrap();
        assert!(s.lookup("src", "alice").await.unwrap().is_none());
    }
}
