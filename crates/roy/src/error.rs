use thiserror::Error;

#[derive(Debug, Error)]
pub enum RoyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to spawn `{cmd}`: {source}")]
    Spawn { cmd: String, source: std::io::Error },

    #[error("agent process exited before the turn finished")]
    ProcessExited,

    #[error("turn timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("project already exists: {name}")]
    ProjectExists { name: String },

    #[error("invalid project name `{name}`: {reason}")]
    InvalidProjectName { name: String, reason: String },
}

pub type Result<T> = std::result::Result<T, RoyError>;
