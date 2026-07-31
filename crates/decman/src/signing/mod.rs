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
//! Canton [`Signature`]"; everything else (loading the key, loading prepared
//! submissions, persisting the signature bundle) is provider-independent and
//! stays in `workflow::contracts::steps::sign`.

pub mod vault_export;

use async_trait::async_trait;
use canton_proto_rs::com::digitalasset::canton::crypto::v30::{Signature, SigningPublicKey};
use tonic::transport::Channel;

use crate::error::Result;

/// Everything a signing backend needs to sign for one party key. Resolved once
/// by the caller from Canton's vault metadata (`VaultService.ListMyKeys`).
pub struct SigningKeyContext {
    /// Canton fingerprint of the key. Goes into [`Signature::signed_by`] and is
    /// how Canton selects the verifying key from the party's topology.
    pub fingerprint: String,
    /// The public key (format, key spec, bytes). A backend uses this to choose
    /// the signature algorithm/format, and the export backend uses it to verify
    /// the recovered private key.
    pub public_key: SigningPublicKey,
    /// The underlying KMS key id, present when the key is KMS-backed (from
    /// `PrivateKeyMetadata.kms_key_id`). A KMS signing backend signs with this;
    /// the export backend ignores it.
    pub kms_key_id: Option<String>,
}

/// Signs a decentralized party's interactive-submission prepared hashes.
///
/// Implement this trait to add a new signing backend (e.g. AWS KMS, MPCH), then
/// wire it into [`select_signer`].
#[async_trait]
pub trait TransactionSigner: Send + Sync {
    /// Sign each prepared-transaction `hash` with the party key described by
    /// `key`, returning one Canton [`Signature`] per hash, in order.
    async fn sign(&self, hashes: &[Vec<u8>], key: &SigningKeyContext) -> Result<Vec<Signature>>;
}

/// Choose the signing backend for a given party key.
///
/// The choice is forced by how the key is held, not by preference: a
/// KMS-backed key can only be signed by the matching KMS backend, an
/// exportable key by [`vault_export::VaultExportSigner`]. Today only the export
/// backend exists; a KMS-backed key still falls through to it and fails at
/// export exactly as before — the KMS backend lands with its own change. This
/// is the single extension point for new backends.
///
/// Takes the caller's already-open admin-API `channel` so the export backend
/// reuses one connection instead of opening a second. Async so a future KMS
/// backend can build its own KMS client here.
pub async fn select_signer(
    _key: &SigningKeyContext,
    channel: Channel,
) -> Result<Box<dyn TransactionSigner>> {
    // TODO(#264 Phase 2): when `_key.kms_key_id.is_some()`, return the KMS
    // backend (AWS KMS / MPCH) instead of the export backend.
    Ok(Box::new(vault_export::VaultExportSigner::new(channel)))
}
