//! Wire-protocol surface shared by the roy daemon and every trigger.
//! Sync, leaf crate: no tokio, no ACP SDK, no rusqlite.

pub mod channel;
pub mod control;
pub mod error;
pub mod event;
pub mod harnesses;
pub mod journal;
pub mod pid_lock;
pub mod wire;

pub use channel::{SessionStrategyWire, TelegramSource};
pub use control::{ClientCommand, ConnectionSpec, ErrorCode, FireTarget, ServerEvent};
pub use error::{Result, RoyError};
pub use event::{event_from_json, event_to_json, StopReason, TurnEvent};
pub use harnesses::{Harness, HarnessInfo, HarnessesConfigStatus, ModelInfo};
pub use journal::{parse_entry_line, JournalEntry, Seq};
pub use pid_lock::{peek_pid, pid_alive, pid_path_for_socket, PidLock};
pub use wire::{decode_line, default_socket_path, encode_line};
