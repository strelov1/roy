//! Read-only facade over the scheduler DB for external consumers (e.g.
//! roy-management's HTTP read endpoints). Wraps the private `store` read fns so
//! callers depend on this stable surface instead of `store` internals, and never
//! run migrations. Pairs with `db::open_read_only`.

use anyhow::Result;
use sqlx::SqlitePool;

use crate::store;
use crate::types::{Agent, Fire, Trigger};

pub struct SchedulerRead {
    pool: SqlitePool,
}

impl SchedulerRead {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list_agents(&self) -> Result<Vec<Agent>> {
        store::agents::list(&self.pool).await
    }

    pub async fn list_triggers(&self, agent: Option<&str>, limit: i64) -> Result<Vec<Trigger>> {
        match agent {
            Some(id) => store::triggers::list_for_agent(&self.pool, id).await,
            None => store::triggers::list_all(&self.pool, limit).await,
        }
    }

    pub async fn list_fires(&self, agent: Option<&str>, limit: i64) -> Result<Vec<Fire>> {
        match agent {
            Some(id) => store::fires::list_for_agent(&self.pool, id, limit).await,
            None => store::fires::list_recent(&self.pool, limit).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db;
    use crate::read::SchedulerRead;
    use crate::store::{agents, triggers};
    use chrono::Utc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn facade_lists_inserted_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.db");

        // Create + migrate via the writer's open(), then populate fixtures.
        let pool = db::open(&path).await.unwrap();
        let agent = agents::insert(
            &pool,
            agents::NewAgent {
                name: "nightly".into(),
                harness: "claude".into(),
                project_id: None,
                task: "summarize".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        triggers::insert_cron(
            &pool,
            triggers::NewCronTrigger {
                agent_id: agent.id.clone(),
                cron_expr: "0 9 * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now(),
            },
        )
        .await
        .unwrap();

        // Open the SAME path read-only (no migration) and read via the facade.
        let ro_pool = db::open_read_only(&path).await.unwrap();
        let read = SchedulerRead::new(ro_pool);

        let agents = read.list_agents().await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, agent.id);

        let all_triggers = read.list_triggers(None, 100).await.unwrap();
        assert_eq!(all_triggers.len(), 1);
        assert_eq!(all_triggers[0].agent_id, agent.id);

        let agent_triggers = read.list_triggers(Some(&agent.id), 100).await.unwrap();
        assert_eq!(agent_triggers.len(), 1);
        assert_eq!(agent_triggers[0].agent_id, agent.id);

        let fires = read.list_fires(None, 100).await.unwrap();
        assert!(fires.is_empty());
    }

    #[tokio::test]
    async fn open_read_only_errors_on_missing_file_without_creating_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.db");
        assert!(!path.exists());

        let err = db::open_read_only(&path).await;
        assert!(
            err.is_err(),
            "opening a nonexistent DB read-only must error"
        );
        assert!(
            !path.exists(),
            "open_read_only must NOT create the file on a missing path"
        );
    }
}
