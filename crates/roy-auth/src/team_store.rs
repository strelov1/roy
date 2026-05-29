use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::types::{NewTeam, Role, Team, TeamMembership};

#[derive(Debug, thiserror::Error)]
pub enum TeamStoreError {
    #[error("team not found: {0}")]
    NotFound(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct TeamStore {
    pool: SqlitePool,
}

impl TeamStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create a team with `owner_id` as the owner. Inserts both rows in one tx.
    pub async fn create(&self, new: NewTeam, owner_id: &str) -> Result<Team, TeamStoreError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp_millis();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO teams (id, name, description, created_by, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&new.name)
        .bind(&new.description)
        .bind(owner_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO team_members (user_id, team_id, role, joined_at)
             VALUES (?, ?, 'owner', ?)",
        )
        .bind(owner_id)
        .bind(&id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Team {
            id,
            name: new.name,
            description: new.description,
            created_by: Some(owner_id.into()),
            created_at: now,
        })
    }

    pub async fn list_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<TeamMembership>, TeamStoreError> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT teams.id, teams.name, team_members.role
             FROM teams INNER JOIN team_members ON team_members.team_id = teams.id
             WHERE team_members.user_id = ?
             ORDER BY teams.created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, role)| TeamMembership {
                id,
                name,
                role: if role == "owner" {
                    Role::Owner
                } else {
                    Role::Member
                },
            })
            .collect())
    }

    pub async fn is_member(&self, user_id: &str, team_id: &str) -> Result<bool, TeamStoreError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM team_members WHERE user_id = ? AND team_id = ?")
                .bind(user_id)
                .bind(team_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    pub async fn is_owner(&self, user_id: &str, team_id: &str) -> Result<bool, TeamStoreError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT role FROM team_members WHERE user_id = ? AND team_id = ?")
                .bind(user_id)
                .bind(team_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(matches!(row, Some((r,)) if r == "owner"))
    }

    pub async fn delete(&self, team_id: &str) -> Result<(), TeamStoreError> {
        let res = sqlx::query("DELETE FROM teams WHERE id = ?")
            .bind(team_id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(TeamStoreError::NotFound(team_id.into()));
        }
        Ok(())
    }

    pub async fn add_member(&self, team_id: &str, user_id: &str) -> Result<(), TeamStoreError> {
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT OR IGNORE INTO team_members (user_id, team_id, role, joined_at)
             VALUES (?, ?, 'member', ?)",
        )
        .bind(user_id)
        .bind(team_id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
