//! Background task that deletes `session_meta` rows whose `session_id` is
//! unknown to the core daemon (neither live nor archived). Runs every 10
//! minutes; disable via `ROY_MGMT_ORPHAN_SWEEP=off`.

use std::sync::Arc;
use std::time::Duration;

use crate::meta_store::MetaStore;
use crate::roy_client::DaemonClient;

pub fn spawn(meta: MetaStore, daemon: Arc<dyn DaemonClient>) {
    if std::env::var("ROY_MGMT_ORPHAN_SWEEP").as_deref() == Ok("off") {
        return;
    }
    tokio::spawn(async move {
        let interval = Duration::from_secs(600);
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = run_once(&meta, &*daemon).await {
                tracing::warn!(error = %e, "orphan_sweep iteration failed");
            }
        }
    });
}

async fn run_once(meta: &MetaStore, daemon: &dyn DaemonClient) -> anyhow::Result<()> {
    // Snapshot meta FIRST, then live + archived. `POST /sessions` writes the
    // meta row strictly *after* the daemon spawn completes; reading meta
    // first means a concurrent spawn either misses this iteration (no row
    // to delete) or appears in the subsequent `daemon.list()` snapshot,
    // never the broken middle state where the row exists in meta but the
    // daemon doesn't yet know the session.
    let all_metas = meta.list_all_session_metas().await?;
    let live = daemon.list().await?;
    let archived = daemon.list_archived().await?;
    let known: std::collections::HashSet<String> = live.into_iter().chain(archived).collect();
    for m in all_metas {
        if !known.contains(&m.session_id) {
            tracing::info!(session = %m.session_id, "sweeping orphan management row");
            let _ = meta.delete_session_meta(&m.session_id).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta_store::SessionMeta;
    use crate::roy_client::mock::MockDaemonClient;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn deletes_unknown_session_meta() {
        let dir = tempfile::tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        roy_auth::apply_migrations(&pool).await.unwrap();
        let user = roy_auth::test_support::make_user(&pool, "alice").await;
        let workspace = dir.path().join("workspace");
        // Leak the tempdir: pool reads from this file for the rest of the test.
        std::mem::forget(dir);
        let meta = MetaStore::new(pool, workspace);
        meta.upsert_session_meta(&SessionMeta {
            session_id: "ghost".into(),
            project_id: None,
            agent_id: None,
            agent_name: None,
            display_label: None,
            created_by: user.id.clone(),
            team_id: None,
            tags: BTreeMap::new(),
            created_at: 1,
            connection_ids: Vec::new(),
        })
        .await
        .unwrap();
        meta.upsert_session_meta(&SessionMeta {
            session_id: "alive".into(),
            project_id: None,
            agent_id: None,
            agent_name: None,
            display_label: None,
            created_by: user.id.clone(),
            team_id: None,
            tags: BTreeMap::new(),
            created_at: 1,
            connection_ids: Vec::new(),
        })
        .await
        .unwrap();

        let mock = MockDaemonClient {
            list_response: std::sync::Mutex::new(Some(vec!["alive".into()])),
            ..Default::default()
        };
        run_once(&meta, &mock).await.unwrap();
        assert!(meta.get_session_meta("ghost").await.unwrap().is_none());
        assert!(meta.get_session_meta("alive").await.unwrap().is_some());
    }
}
