#[derive(Debug, thiserror::Error)]
pub enum AclError {
    #[error("placeholder")]
    Placeholder,
}

pub struct Acl<'a> {
    pub pool: &'a sqlx::SqlitePool,
    pub user_id: &'a str,
}
