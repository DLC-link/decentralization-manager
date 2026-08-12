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

use sha2::{Digest, Sha256};

/// Multihash prefix for SHA-256 hashes in Canton
/// - 0x12 = SHA-256 hash algorithm identifier
/// - 0x20 = 32 bytes (length of SHA-256 output)
pub const MULTIHASH_SHA256_PREFIX: &str = "1220";

/// Canton `HashPurpose.PublicKeyFingerprint` domain-separation constant.
const PURPOSE_PUBLIC_KEY_FINGERPRINT: i32 = 12;

/// Compute the Canton namespace fingerprint for a raw Ed25519 public key.
///
/// SHA-256 over the 4-byte big-endian purpose id (12) followed by the raw key
/// bytes, hex-encoded behind the `1220` SHA-256 multihash prefix. This is the
/// namespace segment of the party id.
pub fn fingerprint_from_public_key(public_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PURPOSE_PUBLIC_KEY_FINGERPRINT.to_be_bytes());
    hasher.update(public_key);
    let hash = hasher.finalize();
    format!("{MULTIHASH_SHA256_PREFIX}{hash}", hash = hex::encode(hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_has_multihash_shape() {
        let fp = fingerprint_from_public_key(&[1u8; 32]);
        assert!(
            fp.starts_with("1220"),
            "fingerprint must carry the SHA-256 multihash prefix: {fp}"
        );
        // "1220" (2-byte prefix) + 32-byte SHA-256, hex-encoded = 68 chars.
        assert_eq!(fp.len(), 68);
        assert!(fp[4..].bytes().all(|b| b.is_ascii_hexdigit()));
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
}
