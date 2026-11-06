use async_trait::async_trait;
use canton_proto_rs::com::digitalasset::canton::crypto::v30::{Signature, SigningPublicKey};

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    signing::{
        SigningError,
        aws_kms::AwsKmsSigner,
        key_store::{KeyStore, StoredKeySigner},
        vault_export::{VaultExportSigner, export_protocol_version},
    },
};

/// An interactive-submission prepared-transaction hash, as returned by the
/// Ledger API `PrepareSubmission` call.
///
/// The bytes stay length-unchecked on purpose. The proto defines the field as
/// free-form `bytes` with no length promise, and Canton signature schemes
/// treat the value as the *message* to sign (the scheme hashes it again
/// internally), so no fixed size can be assumed here.
#[derive(Debug)]
pub struct PreparedTransactionHash(Vec<u8>);

impl PreparedTransactionHash {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Everything a signing backend needs to sign for one party key. Resolved once
/// by the caller from Canton's vault metadata (`VaultService.ListMyKeys`).
pub struct SigningKeyContext {
    /// Canton fingerprint of the key. Goes into [`Signature::signed_by`] and is
    /// how Canton selects the verifying key from the party's topology.
    pub fingerprint: Fingerprint,
    /// The public key (format, key spec, bytes). A backend uses this to choose
    /// the signature algorithm/format, and the export backend uses it to verify
    /// the recovered private key.
    pub public_key: SigningPublicKey,
    /// The underlying KMS key id, present when the key is KMS-backed (from
    /// `PrivateKeyMetadata.kms_key_id`). A KMS signing backend signs with this;
    /// the export backend ignores it.
    ///
    /// `None` is not a failure state: it is the normal value for a non-KMS
    /// key. A failure to *read* the metadata is a hard error at the
    /// `ListMyKeys` call site and never reaches this field.
    pub kms_key_id: Option<String>,
}

/// Signs a decentralized party's interactive-submission prepared hashes.
///
/// Implement this trait to add a new signing backend (e.g. AWS KMS, MPCH), then
/// wire it into [`select_signer`].
#[async_trait]
pub trait TransactionSigner: Send + Sync {
    /// Sign each prepared-transaction hash with the party key described by
    /// `key`, returning one Canton [`Signature`] per hash, in order.
    async fn sign(
        &self,
        hashes: &[PreparedTransactionHash],
        key: &SigningKeyContext,
    ) -> Result<Vec<Signature>, SigningError>;
}

/// Choose the signing backend for a given party key.
///
/// The choice is forced by how the key is held, not by preference:
///
/// 1. A key DecMan holds in its own [`KeyStore`] signs from there
///    ([`StoredKeySigner`]). Canton 3.7 no longer exports vault keys by
///    default, so this is the path that keeps working after the upgrade.
/// 2. A key that carries a `kms_key_id` is non-exportable and can only be
///    signed through the KMS ([`AwsKmsSigner`]).
/// 3. Any other vault key is exported and signed by [`VaultExportSigner`],
///    which also copies it into the store for the next run.
///
/// This is the single extension point for new backends (e.g. an MPCH
/// client-API signer).
///
/// Returns a boxed trait object because the backend is picked at *runtime*
/// from the key's custody, so the concrete type differs per call; the one
/// dynamic dispatch is noise next to the signing RPCs themselves.
///
/// Async because the store is read, and because the KMS backend resolves AWS
/// credentials when it is built.
pub async fn select_signer(
    key: &SigningKeyContext,
    config: &NodeConfig,
    store: &KeyStore,
    party: &CantonId,
) -> Result<Box<dyn TransactionSigner>> {
    if let Some(seed) = store.get(party, &key.fingerprint).await? {
        return Ok(Box::new(StoredKeySigner::new(seed)));
    }
    if key.kms_key_id.is_some() {
        // Only AWS KMS is supported so far. A key held by a different KMS
        // driver (e.g. MPCH) also carries a kms_key_id; its Sign call fails
        // with a clear KMS error until a dedicated backend exists.
        Ok(Box::new(AwsKmsSigner::new().await))
    } else {
        Ok(Box::new(
            VaultExportSigner::new(
                config.admin_channel().await?,
                export_protocol_version(config).await?,
            )
            .storing_into(store.clone(), party.clone()),
        ))
    }
}
