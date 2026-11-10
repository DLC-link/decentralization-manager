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
//! use decman_wallet::{ExternalKeyPair, TenantClient, WalletHost, execute_co_validated};
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
//! let party = execute_co_validated(&hosts, &key, "alice", None).await?;
//! println!("party {} across {} hosts", party.party_id, party.hosts.len());
//! # Ok(())
//! # }
//! ```
//!
//! Onboarding is asynchronous on the Canton side: poll [`statuses`] until every
//! host reports the party hosted.
//!
//! Transacting as the party is not part of DecMan's tenant API. The wallet
//! talks to a participant's JSON Ledger API directly, through
//! [`LedgerClient`]. [`send_transfer`] and [`accept_transfer`] move Token
//! Standard assets, with the instrument's [`RegistryClient`] supplying the
//! choice context. Before the key signs, the wallet recomputes the prepared
//! transaction's hash and checks that its root does what the wallet asked.

mod check;
mod http;
mod text;

pub mod client;
pub mod error;
pub mod flow;
pub mod key;
pub mod ledger;
pub mod registry;
pub mod signer;
pub mod transfer;

pub use client::{HostStatus, TenantClient};
pub use error::{Error, Refusal, Result};
pub use flow::{
    AddedHosts, HostReport, OnboardedParty, WalletHost, add_hosts, execute_co_validated,
    raise_threshold, statuses,
};
pub use http::ClientOptions;
pub use key::ExternalKeyPair;
pub use ledger::{
    DisclosedContract, ExecuteRequest, Executed, Holding, LedgerClient, PrepareRequest,
    PreparedSubmission, TransferInstruction,
};
pub use registry::{ChoiceContext, RegistryClient, TransferFactory};
pub use signer::Signer;
pub use transfer::{
    AMULET_TRANSFER_INSTRUCTION_TEMPLATE, AcceptReceipt, CBTC_TRANSFER_INSTRUCTION_TEMPLATE,
    TransferArgs, TransferReceipt, accept_transfer, send_transfer, unlocked_holdings,
};
