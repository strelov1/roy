//! Wire shape for the connections passed to `roy mcp serve-connections`.
//!
//! Mirrors `roy::control::ConnectionSpec` deliberately — keeping it local
//! avoids a build-time dependency from `roy-mcp` back on whatever crate
//! happens to own daemon-side wire types. The two are serde-compatible by
//! construction.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionSpec {
    pub id: String,
    pub slug: String,
    pub kind: String,
    pub config: serde_json::Value,
    #[serde(default)]
    pub secrets: Option<serde_json::Value>,
}

/// Bundle passed via `--specs <path>` (file holds JSON-encoded `Bundle`) or
/// `--specs-stdin` (read the JSON from stdin's first line before switching
/// to JSON-RPC framing for the rest of the conversation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub session_id: String,
    pub connections: Vec<ConnectionSpec>,
}
