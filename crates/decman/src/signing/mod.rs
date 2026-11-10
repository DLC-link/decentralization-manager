//! Pluggable signing backends for a decentralized party's Daml key.
//!
//! A decentralized party authorizes its Daml transactions (today: deploying the
//! governance-core contract at bootstrap) by signing the Ledger API
//! interactive-submission *prepared-transaction hash* with the party's protocol
//! signing key. Where that key lives — and therefore how it signs — varies:
//!
//! - [`key_store::StoredKeySigner`] signs with DecMan's own copy of the key,
//!   kept encrypted in its database. Canton 3.7 refuses to export a vault key
//!   by default, so DecMan copies each key while the participant still allows
//!   that ([`migration`]).
//! - [`aws_kms::AwsKmsSigner`] signs with a non-exportable key held in AWS KMS
//!   by calling the KMS `Sign` API. It serves KMS-backed participants.
//! - [`vault_export::VaultExportSigner`] pulls the private key out of the
//!   participant's vault (`VaultService.ExportKeyPair`) and signs locally with
//!   Ed25519. It also stores the key for the next run.
//!
//! [`select_signer`] picks the backend. New backends (e.g. an MPCH client-API
//! signer) implement [`TransactionSigner`] and add a branch there; nothing
//! else in the signing flow changes.
//!
//! The one operation that varies is "turn a prepared-transaction hash into a
//! Canton `Signature`"; everything else (loading the key, loading prepared
//! submissions, persisting the signature bundle) is provider-independent and
//! stays in `workflow::contracts::steps::sign`.

pub mod aws_kms;
mod error;
pub mod key_store;
pub mod migration;
mod signer;
pub mod vault_export;

pub use error::SigningError;
pub use signer::{PreparedTransactionHash, SigningKeyContext, TransactionSigner, select_signer};
