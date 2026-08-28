//! What the wallet needs from whatever holds the party's key.
//!
//! [`ExternalKeyPair`](crate::ExternalKeyPair) keeps the seed in this process,
//! which is right for a wallet that generates its own key and wrong for a
//! provider whose keys live in a KMS or an HSM. The flows only ever need two
//! things from a key — its public half, and a signature over a hash the node
//! computed — so they ask for those through this trait rather than for the key
//! itself.
//!
//! Signing is async and fallible because a KMS signs over the network. An
//! in-process key implements it by returning immediately.

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::fingerprint::fingerprint_from_public_key;

use crate::{ExternalKeyPair, error::Result};

/// A holder of an external party's Ed25519 key.
///
/// The private half never crosses this boundary: implementations receive a
/// message and return a signature, so a KMS-backed signer keeps its key where it
/// is and an in-process one keeps its seed in its own struct.
#[async_trait]
pub trait Signer: Send + Sync {
    /// The party's raw 32-byte Ed25519 public key.
    fn public_key_bytes(&self) -> [u8; 32];

    /// Sign `message` — always a hash the node computed and the wallet relayed,
    /// never something this crate derived.
    ///
    /// # Errors
    /// Whatever the backing key store failed with.
    async fn sign(&self, message: &[u8]) -> Result<[u8; 64]>;

    /// The public key, base64-encoded, as the tenant API expects it.
    fn public_key_b64(&self) -> String {
        STANDARD.encode(self.public_key_bytes())
    }

    /// The party's Canton namespace fingerprint, derived from the public key
    /// exactly as the node derives it.
    fn fingerprint(&self) -> String {
        fingerprint_from_public_key(&self.public_key_bytes())
    }

    /// The party id this key produces for `hint`.
    fn party_id(&self, hint: &str) -> String {
        format!("{hint}::{fp}", fp = self.fingerprint())
    }

    /// Sign and base64-encode in one step, which is the only form the wire uses.
    ///
    /// # Errors
    /// Propagates [`Signer::sign`].
    async fn sign_b64_async(&self, message: &[u8]) -> Result<String> {
        Ok(STANDARD.encode(self.sign(message).await?))
    }
}

#[async_trait]
impl Signer for ExternalKeyPair {
    fn public_key_bytes(&self) -> [u8; 32] {
        ExternalKeyPair::public_key_bytes(self)
    }

    /// Infallible: the seed is right here, so there is nothing to fail.
    async fn sign(&self, message: &[u8]) -> Result<[u8; 64]> {
        Ok(ExternalKeyPair::sign(self, message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for a KMS: holds no seed, signs somewhere else, and can fail.
    /// Its existence is the point of the trait — the flows must work with a key
    /// this process cannot read.
    struct RemoteSigner {
        public_key: [u8; 32],
        available: bool,
    }

    #[async_trait]
    impl Signer for RemoteSigner {
        fn public_key_bytes(&self) -> [u8; 32] {
            self.public_key
        }

        async fn sign(&self, _message: &[u8]) -> Result<[u8; 64]> {
            if !self.available {
                return Err(crate::Error::NotEnoughHosts(0));
            }
            Ok([9u8; 64])
        }
    }

    /// A signer that never exposes a seed still derives the same party id an
    /// in-process key would, because both derive it from the public key alone.
    #[tokio::test]
    async fn a_remote_signer_derives_the_same_party_id() {
        let key = ExternalKeyPair::from_seed([3u8; 32]);
        let remote = RemoteSigner {
            public_key: Signer::public_key_bytes(&key),
            available: true,
        };

        assert_eq!(remote.fingerprint(), Signer::fingerprint(&key));
        assert_eq!(remote.party_id("alice"), Signer::party_id(&key, "alice"));
        assert_eq!(remote.public_key_b64(), Signer::public_key_b64(&key));
    }

    /// Signing is fallible so a KMS outage surfaces as an error rather than a
    /// panic or a bogus signature.
    #[tokio::test]
    async fn a_failing_remote_signer_reports_an_error() {
        let remote = RemoteSigner {
            public_key: [1u8; 32],
            available: false,
        };
        assert!(remote.sign(b"hash").await.is_err());
    }

    /// The in-process key still signs, and through the same interface.
    #[tokio::test]
    async fn an_in_process_key_signs_through_the_trait() {
        let key = ExternalKeyPair::from_seed([5u8; 32]);
        let Ok(encoded) = key.sign_b64_async(b"a hash").await else {
            panic!("an in-process key cannot fail to sign");
        };
        assert_eq!(encoded, key.sign_b64(b"a hash"));
    }
}
