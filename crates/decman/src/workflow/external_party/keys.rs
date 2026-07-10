//! Client-side Ed25519 key material for a decentrally-hosted external party.
//!
//! Unlike the node-managed keys the other workflows generate through Canton's
//! `VaultService`, an external party owns its namespace key itself. This module
//! generates that key locally, derives the party's Canton fingerprint/id from
//! it, and signs the onboarding multi-hash — all without touching the node.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::{Rng, rng};
use sha2::{Digest, Sha256};

use crate::utils::MULTIHASH_SHA256_PREFIX;

/// Canton `HashPurpose.PublicKeyFingerprint` domain-separation constant.
const PURPOSE_PUBLIC_KEY_FINGERPRINT: i32 = 12;

/// An externally-held Ed25519 keypair for an external party. The 32-byte seed
/// is the private key; the party's namespace fingerprint is derived from the
/// public key exactly as Canton derives it.
pub struct ExternalKeyPair {
    signing: SigningKey,
}

impl ExternalKeyPair {
    /// Generate a fresh keypair from the thread RNG.
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        rng().fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    /// Reconstruct a keypair from a previously-persisted 32-byte seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// The 32-byte private seed. Handle as a secret.
    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The raw 32-byte Ed25519 public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// The Canton namespace fingerprint of the public key (`1220` multihash
    /// prefix + hex SHA-256 over the domain-separated raw key bytes). This is
    /// the namespace segment of the party id.
    pub fn fingerprint(&self) -> String {
        fingerprint_from_public_key(&self.public_key_bytes())
    }

    /// The external party id: `{hint}::{fingerprint}`.
    pub fn party_id(&self, hint: &str) -> String {
        format!("{hint}::{fp}", fp = self.fingerprint())
    }

    /// Sign `message` (the onboarding multi-hash), returning the raw 64-byte
    /// Ed25519 signature (`r || s`, the `SIGNATURE_FORMAT_CONCAT` encoding).
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }

    /// The public key, for verification by callers and tests.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }
}

/// Compute the Canton namespace fingerprint for a raw Ed25519 public key.
///
/// Mirrors [`crate::utils::compute_fingerprint`] for the raw-key case: SHA-256
/// over the 4-byte big-endian purpose id (12) followed by the raw key bytes,
/// hex-encoded behind the `1220` SHA-256 multihash prefix.
pub fn fingerprint_from_public_key(public_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PURPOSE_PUBLIC_KEY_FINGERPRINT.to_be_bytes());
    hasher.update(public_key);
    let hash = hasher.finalize();
    format!("{MULTIHASH_SHA256_PREFIX}{hash}", hash = hex::encode(hash))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier};

    use super::*;

    #[test]
    fn generate_produces_distinct_keypairs() {
        let a = ExternalKeyPair::generate();
        let b = ExternalKeyPair::generate();
        assert_ne!(a.public_key_bytes(), b.public_key_bytes());
    }

    #[test]
    fn from_seed_is_deterministic() {
        let seed = [7u8; 32];
        let a = ExternalKeyPair::from_seed(seed);
        let b = ExternalKeyPair::from_seed(seed);
        assert_eq!(a.public_key_bytes(), b.public_key_bytes());
        assert_eq!(a.seed(), seed);
    }

    #[test]
    fn fingerprint_has_multihash_shape() {
        let kp = ExternalKeyPair::from_seed([1u8; 32]);
        let fp = kp.fingerprint();
        assert!(
            fp.starts_with("1220"),
            "fingerprint must carry the SHA-256 multihash prefix: {fp}"
        );
        // "1220" (2-byte prefix) + 32-byte SHA-256, hex-encoded = 68 chars.
        assert_eq!(fp.len(), 68);
        assert!(fp[4..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn party_id_is_hint_then_fingerprint() {
        let kp = ExternalKeyPair::from_seed([2u8; 32]);
        let id = kp.party_id("alice");
        let Some((hint, fp)) = id.split_once("::") else {
            panic!("party id must contain '::': {id}");
        };
        assert_eq!(hint, "alice");
        assert_eq!(fp, kp.fingerprint());
    }

    #[test]
    fn sign_verifies_and_rejects_tampering() {
        let kp = ExternalKeyPair::from_seed([3u8; 32]);
        let msg = b"multi-hash-bytes";
        let sig = Signature::from_bytes(&kp.sign(msg));
        assert!(kp.verifying_key().verify(msg, &sig).is_ok());
        // A tampered message must not verify against the same signature.
        assert!(kp.verifying_key().verify(b"tampered", &sig).is_err());
    }

    #[test]
    fn fingerprint_matches_free_function() {
        let kp = ExternalKeyPair::from_seed([9u8; 32]);
        assert_eq!(
            kp.fingerprint(),
            fingerprint_from_public_key(&kp.public_key_bytes())
        );
    }
}
