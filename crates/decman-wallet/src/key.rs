//! The party's Ed25519 namespace key — generated and held by the wallet.
//!
//! DecMan never sees the private key and cannot make one: the node-side library
//! only derives fingerprints from *public* keys. Everything secret lives here,
//! in the wallet's own process.

use base64::{Engine, engine::general_purpose::STANDARD};
use common::fingerprint::fingerprint_from_public_key;
use ed25519_dalek::{Signer, SigningKey};
use rand::{Rng, rng};
use zeroize::Zeroizing;

/// A client-held Ed25519 keypair for an external party. The 32-byte seed is the
/// private key; the party's Canton namespace fingerprint is derived from the
/// public key exactly as the node derives it (via the shared library function in
/// [`common::fingerprint`]).
///
/// `ed25519_dalek::SigningKey` is built with the `zeroize` feature, so the seed
/// is wiped when this is dropped.
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

    /// Reconstruct a keypair from a 32-byte seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// Reconstruct a keypair from a base64-encoded 32-byte seed.
    ///
    /// # Errors
    /// Returns the offending input description if it is not base64 or not 32 bytes.
    pub fn from_seed_b64(seed: &str) -> std::result::Result<Self, String> {
        let bytes = STANDARD
            .decode(seed.trim())
            .map_err(|e| format!("seed is not valid base64: {e}"))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|b: Vec<u8>| format!("seed must be 32 bytes, got {len}", len = b.len()))?;
        Ok(Self::from_seed(seed))
    }

    /// The 32-byte private seed, wrapped so it is wiped after use.
    pub fn seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.signing.to_bytes())
    }

    /// The base64-encoded private seed, wrapped so it is wiped after use. For
    /// wallets that persist the key; never send this anywhere.
    pub fn seed_b64(&self) -> Zeroizing<String> {
        Zeroizing::new(STANDARD.encode(*self.seed()))
    }

    /// The raw 32-byte Ed25519 public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// The raw public key, base64-encoded — the wire form the tenant API takes.
    pub fn public_key_b64(&self) -> String {
        STANDARD.encode(self.public_key_bytes())
    }

    /// The Canton namespace fingerprint of the public key (the `{fingerprint}`
    /// segment of the party id, and the `signed_by` the tenant API expects).
    pub fn fingerprint(&self) -> String {
        fingerprint_from_public_key(&self.public_key_bytes())
    }

    /// The external party id: `{hint}::{fingerprint}`.
    pub fn party_id(&self, hint: &str) -> String {
        format!("{hint}::{fp}", fp = self.fingerprint())
    }

    /// Sign `message`, returning the raw 64-byte Ed25519 signature (`r || s`,
    /// Canton's `SIGNATURE_FORMAT_CONCAT` encoding).
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }

    /// Sign `message` and base64-encode the signature for the wire.
    pub fn sign_b64(&self, message: &[u8]) -> String {
        STANDARD.encode(self.sign(message))
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Verifier, VerifyingKey};

    use super::*;

    #[test]
    fn seed_roundtrips_through_base64() {
        let key = ExternalKeyPair::generate();
        let restored = match ExternalKeyPair::from_seed_b64(&key.seed_b64()) {
            Ok(k) => k,
            Err(e) => panic!("a freshly encoded seed must decode: {e}"),
        };
        assert_eq!(*key.seed(), *restored.seed());
        assert_eq!(key.public_key_bytes(), restored.public_key_bytes());
        assert_eq!(key.fingerprint(), restored.fingerprint());
    }

    #[test]
    fn from_seed_b64_rejects_wrong_length_and_bad_base64() {
        assert!(ExternalKeyPair::from_seed_b64("not base64!!").is_err());
        assert!(ExternalKeyPair::from_seed_b64(&STANDARD.encode([1u8; 31])).is_err());
        assert!(ExternalKeyPair::from_seed_b64(&STANDARD.encode([1u8; 32])).is_ok());
    }

    #[test]
    fn party_id_is_hint_plus_fingerprint() {
        let key = ExternalKeyPair::from_seed([9u8; 32]);
        let fp = key.fingerprint();
        assert_eq!(key.party_id("alice"), format!("alice::{fp}"));
        assert!(
            fp.starts_with("1220"),
            "expected a multihash fingerprint: {fp}"
        );
    }

    /// The signature must verify against the public key the wallet publishes —
    /// this is the pair the node checks the topology bundle with.
    #[test]
    fn signature_verifies_against_the_published_public_key() {
        let key = ExternalKeyPair::generate();
        let message = b"the onboarding multi-hash";
        let signature = key.sign(message);

        let verifying = match VerifyingKey::from_bytes(&key.public_key_bytes()) {
            Ok(v) => v,
            Err(e) => panic!("published public key must be a valid Ed25519 key: {e}"),
        };
        let parsed = ed25519_dalek::Signature::from_bytes(&signature);
        assert!(verifying.verify(message, &parsed).is_ok());
        assert!(verifying.verify(b"a different message", &parsed).is_err());
    }

    #[test]
    fn sign_b64_encodes_the_same_signature() {
        let key = ExternalKeyPair::from_seed([4u8; 32]);
        let message = b"multi-hash";
        assert_eq!(
            key.sign_b64(message),
            STANDARD.encode(key.sign(message)),
            "the base64 helper must not change the signature"
        );
    }

    /// Two wallets must never collide on a party id for the same hint.
    #[test]
    fn distinct_keys_yield_distinct_party_ids() {
        let a = ExternalKeyPair::from_seed([1u8; 32]);
        let b = ExternalKeyPair::from_seed([2u8; 32]);
        assert_ne!(a.party_id("wallet"), b.party_id("wallet"));
    }
}
