use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::password::hash_password;
use crate::types::{NewUser, User};

#[derive(Debug, thiserror::Error)]
pub enum UserStoreError {
    #[error("user not found: {0}")]
    NotFound(String),
    #[error("username already exists")]
    UsernameTaken,
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("bcrypt: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
}

#[derive(Clone)]
pub struct UserStore {
    pool: SqlitePool,
}

impl UserStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, new: NewUser) -> Result<User, UserStoreError> {
        if new.username.trim().is_empty() {
            return Err(UserStoreError::Invalid("username required".into()));
        }
        if new.password.len() < 8 {
            return Err(UserStoreError::Invalid("password too short (min 8)".into()));
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp_millis();
        let hash = hash_password(&new.password)?;
        let res = sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash, timezone, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&new.username)
        .bind(&new.display_name)
        .bind(&hash)
        .bind(&new.timezone)
        .bind(now)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(User {
                id,
                username: new.username,
                display_name: new.display_name,
                password_hash: hash,
                timezone: new.timezone,
                created_at: now,
            }),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                Err(UserStoreError::UsernameTaken)
            }
            Err(e) => Err(UserStoreError::Db(e)),
        }
    }

    pub async fn get(&self, id: &str) -> Result<User, UserStoreError> {
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| UserStoreError::NotFound(id.into()))
    }

    pub async fn get_by_username(&self, username: &str) -> Result<User, UserStoreError> {
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ? COLLATE NOCASE")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| UserStoreError::NotFound(username.into()))
    }

    pub async fn has_any(&self) -> Result<bool, UserStoreError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM users LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn set_password(
        &self,
        user_id: &str,
        new_password: &str,
    ) -> Result<(), UserStoreError> {
        if new_password.len() < 8 {
            return Err(UserStoreError::Invalid("password too short (min 8)".into()));
        }
        let hash = hash_password(new_password)?;
        let res = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(&hash)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(UserStoreError::NotFound(user_id.into()));
        }
        Ok(())
    }

    pub async fn set_timezone(
        &self,
        user_id: &str,
        tz: Option<&str>,
    ) -> Result<(), UserStoreError> {
        sqlx::query("UPDATE users SET timezone = ? WHERE id = ?")
            .bind(tz)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
