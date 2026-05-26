//! `roy-scheduler` — cron + one-shot fire dispatcher for roy.
//!
//! Spec: docs/superpowers/specs/2026-05-23-background-agents-design.md
//!
//! Boundary rule: imports from `roy` only the control protocol
//! (`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`,
//! `StopReason`). Never reaches into Daemon, SessionManager, Engine,
//! Journal, Transport.

pub mod cli;
pub mod db;
pub mod driver;
pub mod plan;
pub mod roy_client;
pub mod store;
pub mod subscribers;
pub mod types;

use std::path::PathBuf;

/// Conventional on-disk location of the scheduler SQLite DB.
/// `$ROY_SCHEDULER_DB` overrides; otherwise `~/.local/state/roy-scheduler/state.db`.
pub fn default_db_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SCHEDULER_DB") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-scheduler/state.db")
}
