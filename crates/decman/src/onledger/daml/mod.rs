//! Daml codec and Ledger API client for the `decman-coordination-v1` package.
//!
//! * [`templates`]: template ids, module paths, choice names, and the
//!   node-level [`templates::CoordinationPackage`] resolver.
//! * [`codec`]: Rust structs mirroring every template and choice argument,
//!   with `Record` encode and decode.
//! * [`client`]: [`client::CoordinationClient`], which submits as the node
//!   party through `CommandService.submit_and_wait_for_transaction` and reads
//!   the ACS as the node party.

pub mod client;
pub mod codec;
pub mod templates;

pub use client::{ActiveContract, CoordinationClient, ExerciseOutcome};
pub use templates::{CoordinationPackage, CoordinationTemplate, choices};
