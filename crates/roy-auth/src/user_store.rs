#[derive(Debug, thiserror::Error)]
pub enum UserStoreError {
    #[error("placeholder")]
    Placeholder,
}

pub struct UserStore;
