pub mod engine;
pub mod error;
pub mod event;
pub mod journal;
pub mod manager;
pub mod provider;
pub mod session;
pub mod transport;

pub use engine::{Attach, EngineOpts, InputLease, SessionEngine};
pub use error::{Result, RoyError};
pub use event::{event_from_json, event_to_json, StopReason, TurnEvent};
pub use journal::{Journal, JournalEntry, Seq};
pub use manager::SessionManager;
pub use provider::{ClaudeProvider, Provider};
pub use session::Session;
pub use transport::{
    AcpConfig, AcpTransport, Handle, PermissionPolicy, PrintTransport, StderrMode, Transport,
};
