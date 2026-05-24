//! Store layer — CRUD per table. Split per table for keep-it-small.
//!
//! All functions take `&SqlitePool` (or `&mut Transaction<'_, Sqlite>`
//! when they must run inside a claim transaction). Timestamps are
//! `DateTime<Utc>` — sqlx serializes them as ISO-8601 TEXT.

pub mod agents;
pub mod fires;
pub mod subscribers;
pub mod triggers;
