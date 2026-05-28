//! Wire shape for the connections passed to `roy mcp serve-connections`.
//!
//! `ConnectionSpec` is the same type the daemon sends down `ClientCommand::Spawn`
//! — we re-export it so callers and tests can write `spec::ConnectionSpec`
//! without reaching across the workspace. `Bundle` is the proxy-only envelope.

use serde::{Deserialize, Serialize};

pub use roy::ConnectionSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub session_id: String,
    pub connections: Vec<ConnectionSpec>,
}
