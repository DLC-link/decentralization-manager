//! Decentrally-hosted external-party onboarding.
//!
//! A sovereign external party whose Ed25519 namespace key is generated and held
//! client-side by the wallet — DPM never sees the private key — hosted with
//! Confirmation permission across N participants at an M-of-N confirmation
//! threshold.
//!
//! There is no inter-DPM coordination: the wallet asks one host to build the
//! multi-host onboarding topology (`prepare_topology`), signs each transaction hash
//! locally, then submits the same signed bundle to each host's
//! `/v0/tenant/onboard`, which allocates it on that host's own participant
//! ([`steps::allocate_party`]). Canton keeps the topology a proposal until the
//! last host signs, so a partial failure leaves a pending party, never a
//! half-created one — the wallet just retries the host that failed.

pub mod add_hosts;
pub mod keys;
pub mod steps;
pub mod threshold;
