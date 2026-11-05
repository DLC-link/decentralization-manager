//! A Daml signing key for a decentralized party that lives for one deploy.
//!
//! Canton 3.7 refuses to export a vault key unless the node sets
//! `enable-key-download`, and its admin API cannot sign arbitrary bytes. So
//! each signer of a contracts run makes a fresh key in memory. The key signs
//! the topology change that adds it to the party, signs the prepared
//! transactions, and is then dropped. No copy is written anywhere.
//!
//! The external-party rule that a node never makes a party key still holds:
//! that rule protects keys a wallet holds, and this key is never held.

use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
    CryptoKeyFormat, Signature, SignatureFormat, SigningAlgorithmSpec, SigningKeySpec,
    SigningKeyUsage, SigningPublicKey,
};
use ed25519_dalek::{Signature as DalekSignature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{Rng, rng};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{
    canton_id::Fingerprint,
    error::Result,
    signing::PreparedTransactionHash,
    utils,
    workflow::external_party::steps::{ED25519_SPKI_DER_PREFIX, ed25519_spki_der},
};

/// Canton's `HashPurpose.TopologyTransactionSignature`.
const TOPOLOGY_HASH_PURPOSE: u32 = 11;

/// Multihash header of a SHA-256 digest: the algorithm code, then the length.
const SHA256_MULTIHASH_HEADER: [u8; 2] = [0x12, 0x20];

/// An Ed25519 key that signs for a decentralized party during one deploy.
///
/// The type has no `Clone`, `Debug` or `Serialize`, so the private half cannot
/// reach a log line, the database or the network by mistake.
/// `ed25519_dalek` wipes it on drop.
pub struct OneShotKey {
    signing: SigningKey,
    public_key: SigningPublicKey,
    fingerprint: Fingerprint,
}

impl OneShotKey {
    /// Make a key from the thread RNG, which is seeded by the OS.
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; 32]);
        rng().fill_bytes(seed.as_mut());
        let signing = SigningKey::from_bytes(&seed);
        let public_key = protocol_public_key(&signing.verifying_key());
        let fingerprint = utils::compute_fingerprint(&public_key);
        Self {
            signing,
            public_key,
            fingerprint,
        }
    }

    pub fn public_key(&self) -> &SigningPublicKey {
        &self.public_key
    }

    pub fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    /// Sign a topology transaction, given as the versioned bytes Canton
    /// receives.
    pub fn sign_topology(&self, transaction: &[u8]) -> Signature {
        self.signature(&topology_hash(transaction))
    }

    /// Sign each prepared-transaction hash, in order.
    pub fn sign_prepared(&self, hashes: &[PreparedTransactionHash]) -> Vec<Signature> {
        hashes
            .iter()
            .map(|hash| self.signature(hash.as_bytes()))
            .collect()
    }

    fn signature(&self, message: &[u8]) -> Signature {
        Signature {
            format: SignatureFormat::Concat as i32,
            signature: self.signing.sign(message).to_bytes().to_vec(),
            signed_by: self.fingerprint.to_string(),
            signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
            signature_delegation: None,
        }
    }
}

/// The public half as a party signing key: Ed25519 in DER
/// `SubjectPublicKeyInfo`, with `Protocol` usage only.
///
/// A decentralized party's namespace belongs to its members, so the key needs
/// no `Namespace` usage. Canton adds the proof-of-ownership usage itself,
/// which lets the key sign its own addition.
fn protocol_public_key(key: &VerifyingKey) -> SigningPublicKey {
    SigningPublicKey {
        format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
        public_key: ed25519_spki_der(&key.to_bytes()),
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        usage: vec![SigningKeyUsage::Protocol as i32],
        ..Default::default()
    }
}

/// The hash Canton signs for a topology transaction: the SHA-256 multihash of
/// the 4-byte purpose and the transaction bytes.
///
/// `transaction` must be the bytes exactly as Canton produced them. Decoding
/// and encoding them again can change the bytes, and with them the hash.
pub fn topology_hash(transaction: &[u8]) -> [u8; 34] {
    let digest = Sha256::new()
        .chain_update(TOPOLOGY_HASH_PURPOSE.to_be_bytes())
        .chain_update(transaction)
        .finalize();
    let mut hash = [0u8; 34];
    hash[..2].copy_from_slice(&SHA256_MULTIHASH_HEADER);
    hash[2..].copy_from_slice(&digest);
    hash
}

/// Read a party signing key that a one-shot signer announced.
///
/// # Errors
///
/// Errors unless the key is an Ed25519 DER `SubjectPublicKeyInfo` with
/// `Protocol` usage and nothing else.
pub fn protocol_verifying_key(key: &SigningPublicKey) -> Result<VerifyingKey> {
    anyhow::ensure!(
        key.format == CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32
            && key.key_spec == SigningKeySpec::EcCurve25519 as i32,
        "Party signing key {fingerprint} is not an Ed25519 SubjectPublicKeyInfo",
        fingerprint = utils::compute_fingerprint(key)
    );
    anyhow::ensure!(
        key.usage == [SigningKeyUsage::Protocol as i32],
        "Party signing key {fingerprint} must have Protocol usage only",
        fingerprint = utils::compute_fingerprint(key)
    );
    let raw: [u8; 32] = key
        .public_key
        .strip_prefix(ED25519_SPKI_DER_PREFIX.as_slice())
        .and_then(|raw| raw.try_into().ok())
        .ok_or_else(|| anyhow::anyhow!("Party signing key is not a DER Ed25519 key"))?;
    Ok(VerifyingKey::from_bytes(&raw)?)
}

/// Check that `signature` is `key`'s Ed25519 signature over `message`.
///
/// # Errors
///
/// Errors when the key is not a one-shot key, when the signature names
/// another key, or when it does not verify.
pub fn verify(key: &SigningPublicKey, message: &[u8], signature: &Signature) -> Result {
    let fingerprint = utils::compute_fingerprint(key);
    anyhow::ensure!(
        signature.signed_by == fingerprint.as_str(),
        "Signature by {signer} was checked against key {fingerprint}",
        signer = signature.signed_by
    );
    let bytes: [u8; 64] = signature
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("Signature by {fingerprint} is not 64 bytes"))?;
    protocol_verifying_key(key)?
        .verify(message, &DalekSignature::from_bytes(&bytes))
        .map_err(|e| anyhow::anyhow!("Signature by {fingerprint} does not verify: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_topology_hash_is_the_multihash_of_the_purpose_and_the_bytes() {
        let transaction = b"versioned topology transaction";
        let mut expected = vec![0x12, 0x20];
        expected.extend_from_slice(
            &Sha256::new()
                .chain_update([0, 0, 0, 11])
                .chain_update(transaction)
                .finalize(),
        );
        assert_eq!(topology_hash(transaction).as_slice(), expected.as_slice());
    }

    #[test]
    fn a_key_announces_protocol_usage_only() -> Result {
        let key = OneShotKey::generate();
        assert_eq!(
            key.public_key().usage,
            vec![SigningKeyUsage::Protocol as i32]
        );
        assert_eq!(key.public_key().public_key.len(), 44);
        protocol_verifying_key(key.public_key())?;
        Ok(())
    }

    #[test]
    fn two_keys_differ() {
        assert_ne!(
            OneShotKey::generate().fingerprint(),
            OneShotKey::generate().fingerprint()
        );
    }

    #[test]
    fn a_topology_signature_covers_the_multihash() -> Result {
        let key = OneShotKey::generate();
        let transaction = b"rotation";
        let signature = key.sign_topology(transaction);
        verify(key.public_key(), &topology_hash(transaction), &signature)?;
        assert!(verify(key.public_key(), transaction, &signature).is_err());
        Ok(())
    }

    #[test]
    fn a_prepared_signature_covers_the_raw_hash() -> Result {
        let key = OneShotKey::generate();
        let hash = PreparedTransactionHash::new(vec![7; 32]);
        let signatures = key.sign_prepared(std::slice::from_ref(&hash));
        verify(key.public_key(), hash.as_bytes(), &signatures[0])?;
        Ok(())
    }

    #[test]
    fn a_signature_does_not_verify_under_another_key() {
        let signer = OneShotKey::generate();
        let other = OneShotKey::generate();
        let signature = signer.sign_topology(b"rotation");
        assert!(verify(other.public_key(), &topology_hash(b"rotation"), &signature).is_err());

        let mut renamed = signature;
        renamed.signed_by = other.fingerprint().to_string();
        assert!(verify(other.public_key(), &topology_hash(b"rotation"), &renamed).is_err());
    }

    #[test]
    fn a_key_with_namespace_usage_is_refused() {
        let mut key = OneShotKey::generate().public_key().clone();
        key.usage.push(SigningKeyUsage::Namespace as i32);
        assert!(protocol_verifying_key(&key).is_err());
    }
}
