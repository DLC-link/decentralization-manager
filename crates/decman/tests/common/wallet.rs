//! Test-only stand-in for a wallet's client-side external-party key.
//!
//! In production the party's Ed25519 namespace key is generated and held by the
//! wallet — DPM never sees the private key and cannot make one (the library only
//! derives fingerprints from *public* keys). The e2e plays that wallet: it
//! generates the key here, signs the onboarding multi-hash locally, and submits
//! only the public key + signature to DPM's tenant API.

use dec_party_manager::workflow::external_party::keys::fingerprint_from_public_key;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::{Rng, rng};

/// A client-held Ed25519 keypair for an external party. The 32-byte seed is the
/// private key; the party's Canton namespace fingerprint is derived from the
/// public key exactly as the node derives it (via the shared library function).
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

    /// Reconstruct a keypair from a 32-byte seed (so a closure can rebuild it).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// The 32-byte private seed.
    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The raw 32-byte Ed25519 public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// The Canton namespace fingerprint of the public key (the `{fingerprint}`
    /// segment of the party id), computed by the shared library function.
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

    /// The public key, for verification in tests.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }
}
