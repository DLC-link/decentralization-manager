use common::fingerprint::MULTIHASH_SHA256_PREFIX;
use sha2::{Digest, Sha256};

/// A SHA-256 multihash over `domain` and `payload`, hex-encoded.
///
/// The length prefix on `domain` keeps two domains from producing the same
/// bytes, so a signature on one kind of payload never verifies as another.
pub fn payload_digest(domain: &str, payload: &str) -> String {
    let domain_length = u32::try_from(domain.len()).unwrap_or(u32::MAX);
    let digest = Sha256::new()
        .chain_update(domain_length.to_be_bytes())
        .chain_update(domain.as_bytes())
        .chain_update(payload.as_bytes())
        .finalize();
    format!("{MULTIHASH_SHA256_PREFIX}{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_known_vector() {
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(3u32.to_be_bytes());
            hasher.update(b"abc");
            hasher.update(b"payload");
            format!("1220{}", hex::encode(hasher.finalize()))
        };
        assert_eq!(payload_digest("abc", "payload"), expected);
    }

    #[test]
    fn domain_separates_digests() {
        assert_ne!(
            payload_digest("ab", "cpayload"),
            payload_digest("abc", "payload")
        );
    }
}
