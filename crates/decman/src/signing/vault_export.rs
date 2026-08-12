//! Signing backend that exports the private key from the participant's vault
//! and signs locally with Ed25519.
//!
//! This is the historical decman behaviour and works only for the JCE crypto
//! provider, whose keys are exportable. It is unusable on a KMS-backed node
//! (the private key cannot leave the KMS) — that case needs a KMS signing
//! backend instead (see [`crate::signing`]).

use async_trait::async_trait;
use canton_proto_rs::com::digitalasset::canton::crypto::{
    admin::v30::{ExportKeyPairRequest, vault_service_client::VaultServiceClient},
    v30::{Signature, SignatureFormat, SigningAlgorithmSpec},
};
use ed25519_dalek::{Signature as DalekSignature, Signer, SigningKey, Verifier, VerifyingKey};
use tonic::transport::Channel;
use zeroize::Zeroizing;

use crate::{
    consts::CANTON_PROTOCOL_VERSION,
    error::Result,
    signing::{PreparedTransactionHash, SigningError, SigningKeyContext, TransactionSigner},
};

/// DER OCTET STRING tag
const DER_OCTET_STRING_TAG: u8 = 0x04;

/// Expected length of Ed25519 private key in bytes (32 bytes)
const ED25519_PRIVATE_KEY_LENGTH: u8 = 0x20;

/// Signs by exporting the private key from Canton's vault and signing locally.
pub struct VaultExportSigner {
    /// Admin-API channel (carries the configured TLS settings), built by
    /// `select_signer` from `NodeConfig::admin_channel`.
    admin_channel: Channel,
}

impl VaultExportSigner {
    pub fn new(admin_channel: Channel) -> Self {
        Self { admin_channel }
    }
}

#[async_trait]
impl TransactionSigner for VaultExportSigner {
    async fn sign(
        &self,
        hashes: &[PreparedTransactionHash],
        key: &SigningKeyContext,
    ) -> Result<Vec<Signature>, SigningError> {
        let key_fingerprint = &key.fingerprint;

        let mut vault_client = VaultServiceClient::new(self.admin_channel.clone());

        // Export the private key.
        tracing::info!("Exporting private key from Canton...");
        tracing::debug!("Key fingerprint: {key_fingerprint}");

        let mut export_response = vault_client
            .export_key_pair(tonic::Request::new(ExportKeyPairRequest {
                fingerprint: key_fingerprint.clone(),
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

        tracing::info!(
            "Searching for valid Ed25519 private key among {max_offset} possible positions"
        );

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
            return Err(anyhow::anyhow!(
                "Could not find any Ed25519 key candidates in exported data"
            )
            .into());
        }

        tracing::info!(
            "Found {count} candidate Ed25519 key positions to try",
            count = candidate_keys.len()
        );

        // Verify each candidate key produces the correct public key.
        tracing::info!("Verifying candidates against expected public key...");

        // Get the public key bytes from Canton's metadata for verification.
        // Canton stores Ed25519 public keys in DER format with this structure:
        // - Bytes 0-11: DER wrapper (SEQUENCE + algorithm OID + BIT STRING header)
        // - Bytes 12-43: Raw 32-byte Ed25519 public key
        let expected_public_key_der = &key.public_key.public_key;

        // Extract raw Ed25519 public key from DER format
        const DER_HEADER_LENGTH: usize = 12;
        const ED25519_PUBLIC_KEY_LENGTH: usize = 32;

        if expected_public_key_der.len() < DER_HEADER_LENGTH + ED25519_PUBLIC_KEY_LENGTH {
            return Err(anyhow::anyhow!(
                "Expected public key is too short: {result_count} bytes (need at least {expected_count})",
                result_count = expected_public_key_der.len(),
                expected_count = DER_HEADER_LENGTH + ED25519_PUBLIC_KEY_LENGTH
            )
            .into());
        }

        let expected_raw_public_key = &expected_public_key_der[DER_HEADER_LENGTH..];

        let mut verified_key_bytes: Option<Zeroizing<[u8; 32]>> = None;

        for (offset, key_bytes, source) in &candidate_keys {
            let signing_key = SigningKey::from_bytes(key_bytes);
            let derived_public_bytes = signing_key.verifying_key().to_bytes();

            // Compare raw Ed25519 public keys (32 bytes)
            if derived_public_bytes.as_slice() == expected_raw_public_key {
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

        // Sign transaction hashes with verified key.
        // `SigningKey` impls `Zeroize` (via the `zeroize` feature on
        // `ed25519-dalek`) and zeros its inner secret on drop.
        tracing::info!(
            "Signing {count} transaction hashes...",
            count = hashes.len()
        );

        let signing_key = SigningKey::from_bytes(&key_bytes);
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
                key_fingerprint,
            )?;
            tracing::info!("Signature {index} verified locally", index = idx + 1);

            // Create Signature protobuf message
            // Ed25519 signatures use CONCAT format (r || s in little-endian)
            signatures.push(Signature {
                format: SignatureFormat::Concat as i32,
                signature: signature_bytes.to_vec(),
                signed_by: key_fingerprint.clone(),
                signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
                signature_delegation: None,
            });
        }

        tracing::debug!("Generated {count} signatures", count = signatures.len());
        Ok(signatures)
    }
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
