//! Pluggable signing backends for a decentralized party's Daml key.
//!
//! A decentralized party authorizes its Daml transactions (today: deploying the
//! governance-core contract at bootstrap) by signing the Ledger API
//! interactive-submission *prepared-transaction hash* with the party's protocol
//! signing key. Where that key lives — and therefore how it signs — varies:
//!
//! - [`vault_export::VaultExportSigner`] pulls the private key out of the
//!   participant's vault (`VaultService.ExportKeyPair`) and signs locally with
//!   Ed25519. This is the only backend today and works for the JCE crypto
//!   provider, whose keys are exportable.
//! - A KMS-backed key (AWS KMS, MPCH) is non-exportable, so it must be signed by
//!   asking the KMS to sign the hash. That backend is a follow-up: implement
//!   [`TransactionSigner`] and add a branch to [`select_signer`]. Nothing else
//!   in the signing flow changes.
//!
//! The one operation that varies is "turn a prepared-transaction hash into a
//! Canton `Signature`"; everything else (loading the key, loading prepared
//! submissions, persisting the signature bundle) is provider-independent and
//! stays in `workflow::contracts::steps::sign`.

mod error;
mod signer;
pub mod vault_export;

pub use error::SigningError;
pub use signer::{PreparedTransactionHash, SigningKeyContext, TransactionSigner, select_signer};
