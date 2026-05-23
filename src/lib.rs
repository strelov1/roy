pub mod error;
pub mod event;
pub mod provider;
pub mod session;
pub mod transport;

pub use error::{Result, RoyError};
pub use event::{StopReason, TurnEvent};
pub use provider::{ClaudeProvider, Provider};
pub use session::Session;
pub use transport::{
    AcpConfig, AcpTransport, Handle, PermissionPolicy, PrintTransport, StderrMode, Transport,
};
