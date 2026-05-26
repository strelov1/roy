//! roy-inbound — event-bus substrate for inbound channels.
//! Spec: docs/superpowers/specs/2026-05-25-inbound-event-bus-design.md

pub mod bus;
pub mod channels;
pub mod config;
pub mod daemon_client;
pub mod reply;
pub mod router;
pub mod session;
pub mod store;
pub mod template;
