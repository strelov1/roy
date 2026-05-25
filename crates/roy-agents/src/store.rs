//! CRUD for the `agents` table. Slugs are derived from the name and made unique
//! by suffixing (`-2`, `-3`, …) on collision.

use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::slug::slugify;
use crate::types::{Agent, AgentUpdate, NewAgent};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a new agent, minting a unique slug from `new.name`. Retries
    /// internally if a concurrent insert wins the SELECT→INSERT race on the
    /// UNIQUE(slug) constraint, so callers never see a raw constraint error.
    pub async fn create(&self, new: NewAgent) -> Result<Agent, StoreError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let base = slugify(&new.name);
        loop {
            let slug = self.unique_slug(&base).await?;
            let res = sqlx::query(
                "INSERT INTO agents
                 (id, name, slug, description, preset, model, prompt, task, persistent, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&new.name)
            .bind(&slug)
            .bind(&new.description)
            .bind(&new.preset)
            .bind(&new.model)
            .bind(&new.prompt)
            .bind(&new.task)
            .bind(new.persistent)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await;
            match res {
                Ok(_) => {
                    return Ok(Agent {
                        id,
                        name: new.name,
                        slug,
                        description: new.description,
                        preset: new.preset,
                        model: new.model,
                        prompt: new.prompt,
                        task: new.task,
                        persistent: new.persistent,
                        created_at: now,
                        updated_at: now,
                    })
                }
                Err(sqlx::Error::Database(d)) if d.is_unique_violation() => continue,
                Err(e) => return Err(StoreError::Db(e)),
            }
        }
    }

    /// Find the first free slug: `base`, then `base-2`, `base-3`, …
    async fn unique_slug(&self, base: &str) -> Result<String, StoreError> {
        let mut candidate = base.to_string();
        let mut n = 1;
        loop {
            let taken: Option<(String,)> = sqlx::query_as("SELECT slug FROM agents WHERE slug = ?")
                .bind(&candidate)
                .fetch_optional(&self.pool)
                .await?;
            if taken.is_none() {
                return Ok(candidate);
            }
            n += 1;
            candidate = format!("{base}-{n}");
        }
    }

    pub async fn get(&self, id: &str) -> Result<Agent, StoreError> {
        sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    /// Look up an agent by its slug. Returns `NotFound` when absent.
    pub async fn get_by_slug(&self, slug: &str) -> Result<Agent, StoreError> {
        sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE slug = ?")
            .bind(slug)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("slug={slug}")))
    }

    pub async fn list(&self) -> Result<Vec<Agent>, StoreError> {
        Ok(
            sqlx::query_as::<_, Agent>("SELECT * FROM agents ORDER BY created_at DESC")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// Apply a partial update. Returns `NotFound` if the id is absent.
    ///
    /// Nullable fields use the double-Option semantics from `AgentUpdate`:
    /// outer `None` leaves the column alone; `Some(inner)` replaces it with
    /// `inner` (which may itself be `None` to clear the column).
    pub async fn update(&self, id: &str, up: AgentUpdate) -> Result<Agent, StoreError> {
        let cur = self.get(id).await?;
        let merged = Agent {
            id: cur.id.clone(),
            slug: cur.slug.clone(),
            created_at: cur.created_at,
            name: up.name.unwrap_or(cur.name),
            description: up.description.unwrap_or(cur.description),
            preset: up.preset.unwrap_or(cur.preset),
            model: up.model.unwrap_or(cur.model),
            prompt: up.prompt.unwrap_or(cur.prompt),
            task: up.task.unwrap_or(cur.task),
            persistent: up.persistent.unwrap_or(cur.persistent),
            updated_at: Utc::now(),
        };
        sqlx::query(
            "UPDATE agents SET name=?, description=?, preset=?, model=?, prompt=?, task=?, persistent=?, updated_at=?
             WHERE id=?",
        )
        .bind(&merged.name)
        .bind(&merged.description)
        .bind(&merged.preset)
        .bind(&merged.model)
        .bind(&merged.prompt)
        .bind(&merged.task)
        .bind(merged.persistent)
        .bind(merged.updated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(merged)
    }

    /// Delete by id. Returns `NotFound` if nothing was removed.
    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM agents WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        // Keep the temp dir alive for the test process lifetime — dropping it
        // would invalidate the SQLite file referenced by the pool.
        std::mem::forget(dir);
        Store::new(pool)
    }

    fn sample(name: &str) -> NewAgent {
        NewAgent {
            name: name.to_string(),
            description: Some("d".into()),
            preset: "claude".into(),
            model: Some("claude-opus-4-7".into()),
            prompt: "You are terse.".into(),
            task: None,
            persistent: false,
        }
    }

    #[tokio::test]
    async fn create_get_list_update_delete() {
        let s = store().await;
        let a = s.create(sample("Reviewer")).await.unwrap();
        assert_eq!(a.slug, "reviewer");
        assert_eq!(s.get(&a.id).await.unwrap().prompt, "You are terse.");
        // builder seed is always present, so list length is seed + 1
        assert_eq!(s.list().await.unwrap().len(), 2);

        let up = AgentUpdate {
            prompt: Some("Be blunt.".into()),
            ..Default::default()
        };
        let updated = s.update(&a.id, up).await.unwrap();
        assert_eq!(updated.prompt, "Be blunt.");
        assert_eq!(updated.slug, "reviewer"); // slug stable

        s.delete(&a.id).await.unwrap();
        assert!(matches!(s.get(&a.id).await, Err(StoreError::NotFound(_))));
        assert!(matches!(
            s.delete(&a.id).await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn slug_collisions_get_suffixed() {
        let s = store().await;
        let a = s.create(sample("Reviewer")).await.unwrap();
        let b = s.create(sample("Reviewer")).await.unwrap();
        assert_eq!(a.slug, "reviewer");
        assert_eq!(b.slug, "reviewer-2");
    }

    #[tokio::test]
    async fn builder_seed_is_present() {
        let s = store().await;
        let b = s.get_by_slug("builder").await.expect("builder seed");
        assert_eq!(b.name, "Agent Builder");
        assert_eq!(b.slug, "builder");
        assert_eq!(b.preset, "claude");
        assert!(b.prompt.contains("Agent Builder"));
        assert!(b.prompt.contains("roy agents update"));
        // After 0003_builder_seed_v2.sql, the LLM is taught the
        // --clear-* flags introduced by the nullable-clear fix.
        assert!(b.prompt.contains("--clear-description"));
        assert!(b.prompt.contains("--clear-model"));
        assert!(b.prompt.contains("--clear-task"));
    }

    #[tokio::test]
    async fn get_by_slug_returns_not_found_for_missing() {
        let s = store().await;
        let err = s.get_by_slug("does-not-exist").await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn update_nullable_clear_via_some_none() {
        let s = store().await;
        let a = s.create(sample("Reviewer")).await.unwrap();
        assert_eq!(a.description.as_deref(), Some("d"));
        assert_eq!(a.model.as_deref(), Some("claude-opus-4-7"));

        // Some(None) clears the column to NULL.
        let up = AgentUpdate {
            description: Some(None),
            model: Some(None),
            ..Default::default()
        };
        let after = s.update(&a.id, up).await.unwrap();
        assert_eq!(after.description, None);
        assert_eq!(after.model, None);

        // A round-trip through the DB also returns None (not stale memory).
        let reread = s.get(&a.id).await.unwrap();
        assert_eq!(reread.description, None);
        assert_eq!(reread.model, None);
    }

    #[tokio::test]
    async fn update_nullable_set_empty_string_keeps_empty() {
        let s = store().await;
        let a = s.create(sample("Reviewer")).await.unwrap();

        // Some(Some("")) writes an empty string, NOT NULL.
        let up = AgentUpdate {
            description: Some(Some(String::new())),
            ..Default::default()
        };
        let after = s.update(&a.id, up).await.unwrap();
        assert_eq!(after.description.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn update_omitting_nullable_leaves_value() {
        let s = store().await;
        let a = s.create(sample("Reviewer")).await.unwrap();
        assert_eq!(a.description.as_deref(), Some("d"));

        // Outer None on description — leave it alone, only touch name.
        let up = AgentUpdate {
            name: Some("Renamed".into()),
            ..Default::default()
        };
        let after = s.update(&a.id, up).await.unwrap();
        assert_eq!(after.name, "Renamed");
        assert_eq!(after.description.as_deref(), Some("d"));
    }

    #[tokio::test]
    async fn update_deserialization_distinguishes_absent_from_null() {
        // Absent field stays the current value; explicit null clears it.
        let absent: AgentUpdate = serde_json::from_str("{}").unwrap();
        assert!(absent.description.is_none());

        let null: AgentUpdate = serde_json::from_str(r#"{"description": null}"#).unwrap();
        assert_eq!(null.description, Some(None));

        let set: AgentUpdate = serde_json::from_str(r#"{"description": "hi"}"#).unwrap();
        assert_eq!(set.description, Some(Some("hi".into())));
    }
}
