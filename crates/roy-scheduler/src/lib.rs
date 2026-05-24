//! `roy-scheduler` — cron + one-shot fire dispatcher for roy.
//!
//! Spec: docs/superpowers/specs/2026-05-23-background-agents-design.md
//!
//! Boundary rule: imports from `roy` only the control protocol
//! (`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`,
//! `StopReason`). Never reaches into Daemon, SessionManager, Engine,
//! Journal, Transport.

pub mod db;
pub mod driver;
pub mod plan;
pub mod roy_client;
pub mod store;
pub mod subscribers;
pub mod types;
