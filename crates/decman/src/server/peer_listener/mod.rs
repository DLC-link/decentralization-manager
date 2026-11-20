//! The always-on Noise listener: one connection per inbound peer message,
//! the shared workflow triggers, the ping loop and the replies to peers.

mod connection;
mod heartbeat;
mod ping;
mod replies;
mod triggers;

pub(crate) use heartbeat::run_heartbeat;
pub(crate) use triggers::WorkflowTriggers;
// The peer failure tests drive a connection directly.
#[cfg(test)]
pub(crate) use connection::handle_incoming_connection;
