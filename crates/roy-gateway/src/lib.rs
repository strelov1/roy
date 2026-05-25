//! Roy → chat-platform gateway. v1 supports a single channel: Telegram.
//!
//! Architecture: one long-lived process per gateway, talks to a running
//! `roy serve` daemon over its Unix socket. Each turn opens a `TurnConn`
//! that drives Spawn/Resume → AcquireInput → Send → Frame stream → ReleaseInput.
//! `(chat_id → roy session_id)` is persisted in a JSON file so chats
//! survive restarts.
//!
//! See `docs/superpowers/plans/2026-05-23-roy-gateway-telegram.md`.

pub mod binder;
pub mod cancel;
pub mod config;
pub mod daemon;
pub mod draft_stream;
pub mod formatting;
pub mod orchestrator;
pub mod telegram;
pub mod typing;
pub mod ws;
