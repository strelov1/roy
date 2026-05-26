use chrono::Utc;
use rand::RngCore;
use sqlx::SqlitePool;

use crate::types::TeamInvite;

#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("invite invalid")]
    Invalid,
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct InviteStore {
    pool: SqlitePool,
}

impl InviteStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        team_id: &str,
        created_by: &str,
        expires_at: Option<i64>,
    ) -> Result<TeamInvite, InviteError> {
        let mut buf = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut buf);
        let token = hex::encode(buf);
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO team_invites (token, team_id, created_by, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&token)
        .bind(team_id)
        .bind(created_by)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(TeamInvite {
            token,
            team_id: team_id.into(),
            created_by: created_by.into(),
            created_at: now,
            expires_at,
            accepted_by: None,
            accepted_at: None,
        })
    }

    /// Accept an invite for `user_id`. All failure modes collapse to
    /// `InviteError::Invalid` (anti-enumeration). On success:
    /// - marks the invite consumed,
    /// - inserts the user into `team_members` (idempotent),
    /// - returns the team_id.
    pub async fn accept(&self, token: &str, user_id: &str) -> Result<String, InviteError> {
        let mut tx = self.pool.begin().await?;
        let row: Option<TeamInvite> = sqlx::query_as("SELECT * FROM team_invites WHERE token = ?")
            .bind(token)
            .fetch_optional(&mut *tx)
            .await?;
        let invite = row.ok_or(InviteError::Invalid)?;
        if invite.accepted_by.is_some() {
            return Err(InviteError::Invalid);
        }
        if let Some(exp) = invite.expires_at {
            if Utc::now().timestamp_millis() > exp {
                return Err(InviteError::Invalid);
            }
        }
        let now = Utc::now().timestamp_millis();
        sqlx::query("UPDATE team_invites SET accepted_by = ?, accepted_at = ? WHERE token = ?")
            .bind(user_id)
            .bind(now)
            .bind(token)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO team_members (user_id, team_id, role, joined_at)
             VALUES (?, ?, 'member', ?)",
        )
        .bind(user_id)
        .bind(&invite.team_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(invite.team_id)
    }
}
