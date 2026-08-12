//! Wallet-side client for DecMan's tenant API — the library a wallet provider
//! embeds to run a **co-validated** Canton party.
//!
//! A co-validated party is hosted on several participants at once but controlled
//! by a single key its owner holds. One key, one signature, N hosts: the owner
//! stays in sole control and gains uptime, because any one host can be down
//! without the party going with it. That is what DecMan's `/v0/tenant/*` API
//! does, and this crate is the client half of it.
//!
//! The security property this crate exists to preserve: **the private key never
//! leaves the process using this library.** DecMan only ever receives a public
//! key and signatures over hashes it computed. Nothing here sends key material
//! anywhere, and the node-side code cannot generate a party key at all.
//!
//! ```no_run
//! use decman_wallet::{ExternalKeyPair, TenantClient, WalletHost, onboard_co_validated};
//! use common::canton_id::CantonId;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! // The hosting set: one DecMan endpoint + participant id per host.
//! let hosts = vec![
//!     WalletHost::new(
//!         TenantClient::new("https://node1.example.com", "tenant-api-key")?,
//!         CantonId::parse("participant::1220aa…")?,
//!     ),
//!     WalletHost::new(
//!         TenantClient::new("https://node2.example.com", "tenant-api-key")?,
//!         CantonId::parse("participant::1220bb…")?,
//!     ),
//! ];
//!
//! // The wallet's key. Generated here, held here, never transmitted.
//! let key = ExternalKeyPair::generate();
//!
//! // Prepare on one host, sign locally, onboard on every host.
//! let party = onboard_co_validated(&hosts, &key, "alice", None).await?;
//! println!("party {} across {} hosts", party.party_id, party.hosts.len());
//! # Ok(())
//! # }
//! ```
//!
//! Onboarding is asynchronous on the Canton side: poll [`statuses`] until every
//! host reports the party hosted, then transact as it.
//!
//! Transacting comes in two shapes. Assets move through the token standard:
//! [`send_transfer`] and [`accept_transfer`] handle Canton Coin and utility
//! instruments like CBTC identically, because the host resolves the instrument's
//! registry and the wallet only signs. Anything else is a plain
//! [`create_contract`] or [`exercise_choice`].

pub mod client;
pub mod error;
pub mod flow;
pub mod key;

pub use client::{HostStatus, TenantClient};
pub use error::{Error, Result};
pub use flow::{
    HostReport, OnboardedParty, WalletHost, accept_transfer, create_contract, exercise_choice,
    onboard_co_validated, send_transfer, statuses,
};
pub use key::ExternalKeyPair;
