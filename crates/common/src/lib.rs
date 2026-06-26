//! Shared types and utilities for the Decentralized Party Manager workspace.
//!
//! This crate holds the wire DTOs and helpers used by both the `decman`
//! server (which produces them) and the `decman-cli` TUI client (which consumes
//! them). It is kept deliberately dependency-light: server-only concerns
//! (sqlx, tonic, actix) stay in `decman`, and the OpenAPI (`utoipa`) schema
//! derives are gated behind the `openapi` feature so clients don't inherit
//! them.

pub mod api;
pub mod canton_id;
pub mod error;
pub mod types;
