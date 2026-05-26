#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("placeholder")]
    Placeholder,
}

pub struct InviteStore;
