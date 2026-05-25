//! roy-management library: agent CRUD HTTP service over the daemon socket.
//! The bin is a thin clap-driven entrypoint over these modules; integration
//! tests link this library directly to exercise the real wire code paths.

pub mod http;
pub mod roy_client;
pub mod state;
