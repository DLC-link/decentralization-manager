//! The peer side of a workflow run: the command loop, its step mapping, and
//! the failure report a peer sends when it gives up.

mod failure;
mod payload;
#[cfg(test)]
mod retry_tests;
mod run;
mod steps;

pub use run::start_peer;
