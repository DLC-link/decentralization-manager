//! Decentralized Party Manager — coordinates Canton "decentralized party"
//! onboarding and governance across participant nodes. Instances communicate
//! with each other over an encrypted Noise channel (coordinator/peer model) and
//! with Canton via its Admin and Ledger gRPC APIs, exposing an HTTP server with
//! an embedded React UI.

pub mod auth;
pub mod canton_id;
pub mod config;
pub mod consts;
pub mod db;
pub mod error;
pub mod noise;
pub mod server;
pub mod utils;
pub mod workflow;
