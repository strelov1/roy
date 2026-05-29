pub mod daemon;
pub mod engine;
pub mod harnesses_config;
pub mod journal;
pub mod manager;
pub mod session_store;
pub mod transport;

// Wire surface lives in roy-protocol; re-export at the historical paths so
// roy-cli, examples, and core tests keep using `roy::...` unchanged.
// NOTE: `wire` is intentionally absent here — a later task adds it.
pub use roy_protocol::{control, error, event, pid_lock};

pub use roy_protocol::control::{
    ClientCommand, ConnectionSpec, ErrorCode, FireTarget, ServerEvent,
};
pub use roy_protocol::error::{Result, RoyError};
pub use roy_protocol::event::{event_from_json, event_to_json, StopReason, TurnEvent};
pub use roy_protocol::pid_lock::{peek_pid, pid_alive, PidLock};

// Types from roy-protocol, actor/loader from core, surfaced via the wrapper modules.
pub use harnesses_config::{Harness, HarnessInfo, HarnessesConfigStatus, ModelInfo};
pub use journal::{ArchivedJournal, Journal, JournalEntry, Seq};

pub use daemon::{Daemon, DefaultTransportFactory, ServeOpts, TransportFactory};
pub use engine::{Attach, EngineOpts, InputLease, SessionEngine, SessionSpawnConfig};
pub use manager::SessionManager;
pub use transport::{AcpConfig, AcpTransport, Handle, PermissionPolicy, Transport};
