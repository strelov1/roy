#[derive(Debug, thiserror::Error)]
pub enum TeamStoreError {
    #[error("placeholder")]
    Placeholder,
}

pub struct TeamStore;
