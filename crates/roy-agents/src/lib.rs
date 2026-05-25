//! Shared agent store: the canonical agent identity (persona + optional task)
//! used by roy-management and, later, roy-scheduler.

pub mod db;
pub mod slug;
pub mod store;
pub mod types;

pub use db::{default_db_path, open};
pub use slug::slugify;
pub use store::{Store, StoreError};
pub use types::{Agent, AgentUpdate, NewAgent};
