//! Canton namespace-fingerprint derivation for external-party keys.
//!
//! The derivation itself lives in [`common::fingerprint`] so the wallet-side
//! client (`decman-wallet`) computes party ids exactly the way this node
//! validates them. Key generation and signing live entirely on the client — a
//! production binary cannot make or hold a party key, so nothing of the sort
//! appears here.

pub use common::fingerprint::fingerprint_from_public_key;
