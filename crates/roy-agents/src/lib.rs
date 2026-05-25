//! Shared agent store: the canonical agent identity (persona + optional task)
//! used by roy-management and, later, roy-scheduler.
//!
//! Types, slug derivation, and the CRUD store are added in subsequent tasks.

pub mod db;

pub use db::{default_db_path, open};
