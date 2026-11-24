//! Canton namespace-fingerprint derivation for external-party keys.
//!
//! An external party owns its Ed25519 namespace key itself; the private key
//! never touches the node. The only thing a node needs from that key is the
//! deterministic Canton fingerprint of its *public* half, to derive the party
//! id and to cross-check the `signed_by` a wallet submits.
//!
//! This lives in `common` because both sides of the tenant API need the exact
//! same derivation: the server (to validate what a wallet sent) and the wallet
//! client (to know its own party id before the server has ever seen the key).
//! Two copies of this would silently disagree.

use std::collections::HashSet;

use sha2::{Digest, Sha256};

use crate::canton_id::Fingerprint;

/// Multihash prefix for SHA-256 hashes in Canton
/// - 0x12 = SHA-256 hash algorithm identifier
/// - 0x20 = 32 bytes (length of SHA-256 output)
pub const MULTIHASH_SHA256_PREFIX: &str = "1220";

/// Canton `HashPurpose.PublicKeyFingerprint` domain-separation constant.
const PURPOSE_PUBLIC_KEY_FINGERPRINT: i32 = 12;

/// Canton `HashPurpose.DecentralizedNamespaceNamespace` domain-separation constant.
const PURPOSE_DECENTRALIZED_NAMESPACE: i32 = 37;

/// Compute the Canton namespace fingerprint for a raw Ed25519 public key.
///
/// SHA-256 over the 4-byte big-endian purpose id (12) followed by the raw key
/// bytes, hex-encoded behind the `1220` SHA-256 multihash prefix. This is the
/// namespace segment of the party id.
pub fn fingerprint_from_public_key(public_key: &[u8]) -> Fingerprint {
    let mut hasher = Sha256::new();
    hasher.update(PURPOSE_PUBLIC_KEY_FINGERPRINT.to_be_bytes());
    hasher.update(public_key);
    Fingerprint::from_sha256(hasher.finalize().into())
}

/// Compute decentralized namespace from individual namespaces
///
/// Canton uses domain-separated hashing with HashPurpose.DecentralizedNamespaceNamespace = 37.
/// Following Canton's HashBuilder protocol, each namespace string is added with a length prefix
/// to prevent hash collisions from concatenation.
///
/// The algorithm:
/// 1. Hash the purpose ID (37) as 4-byte big-endian
/// 2. For each sorted namespace (as hex string):
///    - Hash the length of the UTF-8 encoded string as 4-byte big-endian
///    - Hash the UTF-8 bytes of the string itself
///
/// The decentralized namespace is returned in multihash format with "1220" prefix
pub fn compute_decentralized_namespace(namespaces: &HashSet<String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PURPOSE_DECENTRALIZED_NAMESPACE.to_be_bytes());

    let mut sorted_namespaces: Vec<_> = namespaces.iter().collect();
    sorted_namespaces.sort();

    for namespace in sorted_namespaces {
        let namespace_bytes = namespace.as_bytes();
        let length = namespace_bytes.len() as i32;
        hasher.update(length.to_be_bytes());
        hasher.update(namespace_bytes);
    }

    format!(
        "{MULTIHASH_SHA256_PREFIX}{hash}",
        hash = hex::encode(hasher.finalize())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_has_multihash_shape() -> crate::error::Result {
        let fp = fingerprint_from_public_key(&[1u8; 32]);
        assert_eq!(fp.as_str().parse::<Fingerprint>()?, fp);
        Ok(())
    }

    #[test]
    fn fingerprint_is_deterministic_and_key_sensitive() {
        assert_eq!(
            fingerprint_from_public_key(&[2u8; 32]),
            fingerprint_from_public_key(&[2u8; 32])
        );
        assert_ne!(
            fingerprint_from_public_key(&[2u8; 32]),
            fingerprint_from_public_key(&[3u8; 32])
        );
    }

    /// Pins the derivation against a hand-computed vector: any change to the
    /// purpose id, byte order, or prefix would move a live party's id.
    #[test]
    fn fingerprint_matches_known_vector() {
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(12i32.to_be_bytes());
            hasher.update([7u8; 32]);
            format!("1220{}", hex::encode(hasher.finalize()))
        };
        assert_eq!(fingerprint_from_public_key(&[7u8; 32]), expected);
    }

    /// Pins the decentralized namespace against a hand-computed vector, so
    /// the server and the dApp keep deriving the same party id.
    #[test]
    fn decentralized_namespace_matches_known_vector() {
        let owners = ["1220bb", "1220aa"];
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(37i32.to_be_bytes());
            for owner in ["1220aa", "1220bb"] {
                hasher.update(6i32.to_be_bytes());
                hasher.update(owner.as_bytes());
            }
            format!("1220{}", hex::encode(hasher.finalize()))
        };
        let set: HashSet<String> = owners.iter().map(|o| o.to_string()).collect();
        assert_eq!(compute_decentralized_namespace(&set), expected);
    }
}
