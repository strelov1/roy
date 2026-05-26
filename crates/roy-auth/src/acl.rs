//! Permission checks expressed as guard methods. Each method returns Ok(()) on
//! success and AclError::Forbidden otherwise. Callers run them before any FS
//! or DB write.

use sqlx::SqlitePool;

use crate::team_store::TeamStore;
use crate::types::Scope;

#[derive(Debug, thiserror::Error)]
pub enum AclError {
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

pub struct Acl<'a> {
    pub pool: &'a SqlitePool,
    pub user_id: &'a str,
}

impl<'a> Acl<'a> {
    pub fn new(pool: &'a SqlitePool, user_id: &'a str) -> Self {
        Self { pool, user_id }
    }

    pub async fn can_access_scope(&self, scope: &Scope) -> Result<(), AclError> {
        match scope {
            Scope::Personal => Ok(()),
            Scope::Team { team_id } => {
                let ok = TeamStore::new(self.pool.clone())
                    .is_member(self.user_id, team_id)
                    .await
                    .map_err(|_| AclError::Forbidden)?;
                if ok {
                    Ok(())
                } else {
                    Err(AclError::Forbidden)
                }
            }
        }
    }

    pub async fn can_admin_team(&self, team_id: &str) -> Result<(), AclError> {
        let ok = TeamStore::new(self.pool.clone())
            .is_owner(self.user_id, team_id)
            .await
            .map_err(|_| AclError::Forbidden)?;
        if ok {
            Ok(())
        } else {
            Err(AclError::Forbidden)
        }
    }

    /// A project is accessible by the user when it belongs to a team the user
    /// is a member of, or when the user is the personal owner (team_id NULL).
    /// Returns NotFound if the project id doesn't exist.
    pub async fn can_access_project(&self, project_id: &str) -> Result<(), AclError> {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT created_by, team_id FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_optional(self.pool)
                .await?;
        let (created_by, team_id) = row.ok_or(AclError::NotFound)?;
        if let Some(team_id) = team_id {
            self.can_access_scope(&Scope::Team { team_id }).await
        } else if created_by == self.user_id {
            Ok(())
        } else {
            Err(AclError::Forbidden)
        }
    }
}
