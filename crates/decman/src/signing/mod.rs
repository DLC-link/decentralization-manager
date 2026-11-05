//! Signing for a decentralized party's Daml transactions.
//!
//! A decentralized party authorizes its Daml transactions (today: deploying the
//! governance-core contract at bootstrap) by signing the Ledger API
//! interactive-submission *prepared-transaction hash* with the party's protocol
//! signing keys.
//!
//! The contracts workflow signs with [`one_shot::OneShotKey`]: each signer makes
//! a key for one deploy, and the run makes those keys the party's signing keys.
//! Canton 3.7 no longer exports vault keys by default, and its admin API cannot
//! sign arbitrary bytes, so a vault key cannot sign a prepared hash.
//!
//! [`vault_export::VaultExportSigner`] and [`aws_kms::AwsKmsSigner`] signed
//! with the vault key, and [`select_signer`] chose between them. The contracts
//! workflow no longer calls them.

pub mod aws_kms;
mod error;
pub mod one_shot;
mod signer;
pub mod vault_export;

pub use error::SigningError;
pub use signer::{PreparedTransactionHash, SigningKeyContext, TransactionSigner, select_signer};
