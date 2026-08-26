//! Decentralized Party Manager — coordinates Canton "decentralized party"
//! onboarding and governance across participant nodes. Instances communicate
//! with each other over an encrypted Noise channel (coordinator/peer model) and
//! with Canton via its Admin and Ledger gRPC APIs, exposing an HTTP server with
//! an embedded React UI.

pub mod auth;
pub mod build_info;
pub mod config;
pub mod consts;
pub mod db;
pub mod error;
pub mod noise;
// The `dec-party-manager` and `gen-types` binaries, plus the `tests/` integration
// suite, are separate crates from this lib and reach into `server::` (e.g.
// `server::start_server`, the `gen-types` wire DTOs, `server::GovernanceResponse`),
// so this module path must stay `pub`.
pub mod server;
pub mod signing;
pub mod utils;
pub mod workflow;

pub use common::canton_id;
