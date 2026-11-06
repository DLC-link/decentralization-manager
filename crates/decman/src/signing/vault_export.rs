//! Signing backend that exports the private key from the participant's vault
//! and signs locally with Ed25519.
//!
//! This is the supported path for the JCE crypto provider, whose keys are
//! exportable; `docs/KMS_SIGNING.md` records why it stays. It is unusable on a
//! KMS-backed node (the private key cannot leave the KMS) — that case needs a
//! KMS signing backend instead (see [`crate::signing`]).

use async_trait::async_trait;
use canton_proto_rs::com::digitalasset::canton::crypto::{
    admin::v30::{ExportKeyPairRequest, vault_service_client::VaultServiceClient},
    v30::{Signature, SignatureFormat, SigningAlgorithmSpec, SigningPublicKey},
};
use ed25519_dalek::{Signature as DalekSignature, Signer, SigningKey, Verifier, VerifyingKey};
use tonic::transport::Channel;
use zeroize::Zeroizing;

use crate::{
    canton_id::CantonId,
    consts::CANTON_PROTOCOL_VERSION,
    error::Result,
    signing::{
        PreparedTransactionHash, SigningError, SigningKeyContext, TransactionSigner,
        key_store::KeyStore,
    },
};

/// DER OCTET STRING tag
const DER_OCTET_STRING_TAG: u8 = 0x04;

/// Expected length of Ed25519 private key in bytes (32 bytes)
const ED25519_PRIVATE_KEY_LENGTH: u8 = 0x20;

/// Signs by exporting the private key from Canton's vault and signing locally.
/// When it has a key store, it also stores the exported key there, so later
/// deploys sign without exporting again.
pub struct VaultExportSigner {
    /// Admin-API channel (carries the configured TLS settings), built by
    /// `select_signer` from `NodeConfig::admin_channel`.
    admin_channel: Channel,
    store: Option<(KeyStore, CantonId)>,
}

impl VaultExportSigner {
    pub fn new(admin_channel: Channel) -> Self {
        Self {
            admin_channel,
            store: None,
        }
    }

    /// Keep the exported key in `store` as `party`'s key.
    pub fn storing_into(mut self, store: KeyStore, party: CantonId) -> Self {
        self.store = Some((store, party));
        self
    }
}

#[async_trait]
impl TransactionSigner for VaultExportSigner {
    async fn sign(
        &self,
        hashes: &[PreparedTransactionHash],
        key: &SigningKeyContext,
    ) -> Result<Vec<Signature>, SigningError> {
        let seed = export_private_key(&self.admin_channel, key).await?;
        if let Some((store, party)) = &self.store {
            // Signing goes ahead either way; the next run exports again.
            match store
                .put(party, &key.fingerprint, &key.public_key, &seed)
                .await
            {
                Ok(true) => tracing::info!(
                    "Stored the exported key {fingerprint} in DecMan's key store",
                    fingerprint = key.fingerprint
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    "Could not store the exported key {fingerprint}: {e:#}",
                    fingerprint = key.fingerprint
                ),
            }
        }
        sign_with_seed(&seed, hashes, key)
    }
}

/// Export a party key's Ed25519 seed from the participant's vault, and check
/// that it produces the key's public half.
///
/// # Errors
///
/// Errors when Canton refuses the export, which Canton 3.7 does unless the
/// node sets `canton.parameters.enable-key-download`, or when no 32-byte
/// sequence in the export produces the public key.
pub async fn export_private_key(
    admin_channel: &Channel,
    key: &SigningKeyContext,
) -> Result<Zeroizing<[u8; 32]>, SigningError> {
    let key_fingerprint = &key.fingerprint;

    let mut vault_client = VaultServiceClient::new(admin_channel.clone());

    // Export the private key.
    tracing::info!("Exporting private key from Canton...");
    tracing::debug!("Key fingerprint: {key_fingerprint}");

    let mut export_response = vault_client
        .export_key_pair(tonic::Request::new(ExportKeyPairRequest {
            fingerprint: key_fingerprint.to_string(),
            protocol_version: CANTON_PROTOCOL_VERSION,
            password: String::new(), // Empty: the exported key pair is not passphrase-protected.
        }))
        .await
        .map_err(|e| {
            tracing::error!("ExportKeyPair RPC failed with error: {e:?}");
            tracing::error!("Attempted fingerprint: {key_fingerprint}");
            e
        })?
        .into_inner();

    // Extract Ed25519 private key from Canton's export response.
    // Canton returns the key in a custom format with embedded metadata.
    //
    // Move the bytes directly out of the proto struct with `std::mem::take`
    // into a zeroizing buffer — avoids a second heap copy of the secret that
    // `.clone()` would create. The proto's `key_pair` is left as an empty
    // `Vec` and dropped along with `export_response` shortly after. All
    // 32-byte candidates derived below are also held in `Zeroizing<[u8; 32]>`
    // so they self-wipe on drop.
    let exported_key_data: Zeroizing<Vec<u8>> =
        Zeroizing::new(std::mem::take(&mut export_response.key_pair));
    tracing::debug!(
        "Parsing exported key pair ({len} bytes)",
        len = exported_key_data.len()
    );

    // Strategy: Try ALL possible 32-byte sequences and test each one.
    // The correct private key should verify against the public key.
    let key_size = ED25519_PRIVATE_KEY_LENGTH as usize;
    let max_offset = exported_key_data.len().saturating_sub(key_size);

    tracing::info!("Searching for valid Ed25519 private key among {max_offset} possible positions");

    let mut candidate_keys: Vec<(usize, Zeroizing<[u8; 32]>, &str)> = Vec::new();

    // First, try DER-tagged sequences (0x04 0x20 pattern)
    for offset in 0..max_offset.saturating_sub(2) {
        if exported_key_data[offset] == DER_OCTET_STRING_TAG
            && exported_key_data[offset + 1] == ED25519_PRIVATE_KEY_LENGTH
        {
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&exported_key_data[offset + 2..offset + 2 + key_size]);
            candidate_keys.push((offset + 2, Zeroizing::new(key_bytes), "DER-tagged"));
        }
    }
    tracing::debug!(
        "Found {count} DER-tagged candidates",
        count = candidate_keys.len()
    );

    if candidate_keys.is_empty() {
        tracing::warn!("No DER-tagged sequences found, trying all possible 32-byte sequences");

        // Try every possible 32-byte sequence in the exported data
        for offset in (0..max_offset).step_by(4) {
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&exported_key_data[offset..offset + key_size]);
            candidate_keys.push((offset, Zeroizing::new(key_bytes), "raw"));
        }

        tracing::debug!(
            "Found {count} raw 32-byte candidates",
            count = candidate_keys.len()
        );
    }

    if candidate_keys.is_empty() {
        return Err(
            anyhow::anyhow!("Could not find any Ed25519 key candidates in exported data").into(),
        );
    }

    tracing::info!(
        "Found {count} candidate Ed25519 key positions to try",
        count = candidate_keys.len()
    );

    // Verify each candidate key produces the correct public key.
    tracing::info!("Verifying candidates against expected public key...");

    let expected_raw_public_key = ed25519_public_key(&key.public_key)?;

    let mut verified_key_bytes: Option<Zeroizing<[u8; 32]>> = None;

    for (offset, key_bytes, source) in &candidate_keys {
        let signing_key = SigningKey::from_bytes(key_bytes);
        let derived_public_bytes = signing_key.verifying_key().to_bytes();

        // Compare raw Ed25519 public keys (32 bytes)
        if derived_public_bytes == expected_raw_public_key {
            tracing::info!("Found matching private key at offset {offset} ({source})");
            verified_key_bytes = Some(Zeroizing::new(**key_bytes));
            break;
        }
    }

    let key_bytes = verified_key_bytes.ok_or_else(|| {
        anyhow::anyhow!(
            "None of the {count} candidate keys produced the expected public key. \
                This indicates the private key is not in the expected format in the exported data.",
            count = candidate_keys.len()
        )
    })?;
    // Drop the remaining candidates; each Zeroizing<[u8; 32]> wipes on drop.
    drop(candidate_keys);

    tracing::info!("Successfully verified Ed25519 private key");
    Ok(key_bytes)
}

/// The raw 32-byte Ed25519 key inside a DER `SubjectPublicKeyInfo`.
///
/// # Errors
///
/// Errors when the key is shorter than an Ed25519 `SubjectPublicKeyInfo`.
pub fn ed25519_public_key(key: &SigningPublicKey) -> Result<[u8; 32]> {
    // Bytes 0-11 are the DER wrapper (SEQUENCE, algorithm OID, BIT STRING
    // header); the raw key follows.
    const DER_HEADER_LENGTH: usize = 12;
    key.public_key
        .get(DER_HEADER_LENGTH..)
        .and_then(|raw| <[u8; 32]>::try_from(raw).ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Expected a 44-byte Ed25519 public key, got {count} bytes",
                count = key.public_key.len()
            )
        })
}

/// Sign each prepared-transaction hash with the Ed25519 key `seed`, and verify
/// each signature locally against the key's public half.
///
/// # Errors
///
/// Errors when a signature fails local verification.
pub fn sign_with_seed(
    seed: &[u8; 32],
    hashes: &[PreparedTransactionHash],
    key: &SigningKeyContext,
) -> Result<Vec<Signature>, SigningError> {
    let key_fingerprint = &key.fingerprint;
    // Sign transaction hashes with verified key.
    // `SigningKey` impls `Zeroize` (via the `zeroize` feature on
    // `ed25519-dalek`) and zeros its inner secret on drop.
    tracing::info!(
        "Signing {count} transaction hashes...",
        count = hashes.len()
    );

    let signing_key = SigningKey::from_bytes(seed);
    let verifying_key = signing_key.verifying_key();
    tracing::debug!("Key fingerprint used in signatures: {key_fingerprint}");

    // Sign each prepared transaction hash and create Signature protobuf messages
    let mut signatures: Vec<Signature> = Vec::with_capacity(hashes.len());

    for (idx, hash) in hashes.iter().enumerate() {
        let signature_bytes = signing_key.sign(hash.as_bytes()).to_bytes();

        // Verify locally before submission. A signature that fails here is
        // guaranteed to be rejected at ExecuteSubmission, so fail loudly with
        // context instead of submitting it and getting an opaque ledger error.
        verify_local_signature(
            &verifying_key,
            hash.as_bytes(),
            &signature_bytes,
            idx,
            key_fingerprint.as_str(),
        )?;
        tracing::info!("Signature {index} verified locally", index = idx + 1);

        // Create Signature protobuf message
        // Ed25519 signatures use CONCAT format (r || s in little-endian)
        signatures.push(Signature {
            format: SignatureFormat::Concat as i32,
            signature: signature_bytes.to_vec(),
            signed_by: key_fingerprint.to_string(),
            signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
            signature_delegation: None,
        });
    }

    tracing::debug!("Generated {count} signatures", count = signatures.len());
    Ok(signatures)
}

/// Verify one produced Ed25519 signature against the party's public key before
/// submission. A signature that fails here is guaranteed to be rejected at
/// `ExecuteSubmission`, so this returns an error with context instead of letting
/// the caller submit it and get an opaque ledger error. This mirrors the KMS
/// backend, which also hard-fails on a failed local verification.
fn verify_local_signature(
    verifying_key: &VerifyingKey,
    message: &[u8],
    signature_bytes: &[u8; 64],
    index: usize,
    fingerprint: &str,
) -> Result<(), SigningError> {
    let sig = DalekSignature::from_bytes(signature_bytes);
    verifying_key.verify(message, &sig).map_err(|e| {
        anyhow::anyhow!(
            "Signature {index} failed local verification against the registered \
             public key {fingerprint}: {e}",
            index = index + 1
        )
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_from_seed(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn accepts_a_correct_signature() -> Result<(), SigningError> {
        let signing_key = key_from_seed(1);
        let verifying_key = signing_key.verifying_key();
        let message = b"prepared-transaction-hash";
        let signature = signing_key.sign(message).to_bytes();

        verify_local_signature(&verifying_key, message, &signature, 0, "fingerprint")
    }

    #[test]
    fn rejects_a_tampered_signature() {
        let signing_key = key_from_seed(1);
        let verifying_key = signing_key.verifying_key();
        let message = b"prepared-transaction-hash";
        let mut signature = signing_key.sign(message).to_bytes();
        signature[0] ^= 0xff;

        assert!(
            verify_local_signature(&verifying_key, message, &signature, 0, "fingerprint").is_err()
        );
    }

    #[test]
    fn rejects_a_signature_from_a_different_key() {
        let signer = key_from_seed(1);
        let other_key = key_from_seed(2);
        let verifying_key = other_key.verifying_key();
        let message = b"prepared-transaction-hash";
        let signature = signer.sign(message).to_bytes();

        assert!(
            verify_local_signature(&verifying_key, message, &signature, 0, "fingerprint").is_err()
        );
    }
}
