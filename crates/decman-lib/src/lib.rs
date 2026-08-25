//! Domain model and gRPC codecs for the decman on-chain governance protocol.
//!
//! `framework` is template-agnostic: traits, encode/record toolkits, event
//! filters, the command envelope. `catalog` is decman's protocol content:
//! the by-value `ActionType` enum, one struct per proposal, template
//! accessors, flow builders, and state interpretation.
//!
//! The crate does no I/O. Clocks, randomness, registry contexts, and
//! resolved package refs enter as parameters.

pub mod catalog;
pub mod error;
pub mod framework;

pub use error::Error;
