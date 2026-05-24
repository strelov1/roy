//! Roy → chat-platform gateway. v1 supports a single channel: Telegram.
//!
//! Architecture: one long-lived process per gateway, talks to a running
//! `roy serve` daemon over its Unix socket using `ClientCommand::Fire`.
//! `(chat_id → roy session_id)` is persisted in a JSON file so chats
//! survive restarts.
//!
//! See `docs/superpowers/plans/2026-05-23-roy-gateway-telegram.md`.

pub mod binder;
pub mod config;
pub mod daemon;
pub mod orchestrator;
pub mod telegram;
