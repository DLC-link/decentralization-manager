use ed25519_dalek::{Signature, VerifyingKey};

use crate::Error;

/// Parses a hex-encoded raw 32-byte Ed25519 public key.
///
/// Refuses a small-order key, which a signature can match for almost any
/// message. Also refuses an encoding that is not the point's canonical one:
/// decompression accepts some of those, and the namespace fingerprint covers
/// the bytes, so two encodings of one key would give two namespaces.
pub fn parse_public_key(public_key_hex: &str) -> Result<VerifyingKey, Error> {
    let bytes: [u8; 32] = hex::decode(public_key_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Error::InvalidPublicKey)?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|_| Error::InvalidPublicKey)?;
    let canonical = key.to_edwards().compress().to_bytes() == bytes;
    if key.is_weak() || !canonical {
        return Err(Error::InvalidPublicKey);
    }
    Ok(key)
}

/// Whether `signature_hex` is a valid Ed25519 signature of the bytes that
/// `digest_hex` encodes.
///
/// Malformed input is an error. A well-formed signature that does not match
/// returns `false`.
pub fn verify_signature(
    public_key_hex: &str,
    digest_hex: &str,
    signature_hex: &str,
) -> Result<bool, Error> {
    let key = parse_public_key(public_key_hex)?;
    let message = hex::decode(digest_hex).map_err(|_| Error::InvalidDigest)?;
    let signature: [u8; 64] = hex::decode(signature_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Error::InvalidSignature)?;
    Ok(key
        .verify_strict(&message, &Signature::from_bytes(&signature))
        .is_ok())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    fn signed(seed: u8, message: &[u8]) -> (String, String) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let signature = key.sign(message);
        (
            hex::encode(key.verifying_key().as_bytes()),
            hex::encode(signature.to_bytes()),
        )
    }

    #[test]
    fn accepts_the_signers_signature() -> Result<(), Error> {
        let (public_key, signature) = signed(1, &[0x12, 0x20, 0xab]);
        assert!(verify_signature(&public_key, "1220ab", &signature)?);
        Ok(())
    }

    #[test]
    fn rejects_another_message() -> Result<(), Error> {
        let (public_key, signature) = signed(1, &[0x12, 0x20, 0xab]);
        assert!(!verify_signature(&public_key, "1220ac", &signature)?);
        Ok(())
    }

    #[test]
    fn rejects_another_signer() -> Result<(), Error> {
        let (_, signature) = signed(1, &[0x12, 0x20, 0xab]);
        let (other_key, _) = signed(2, &[]);
        assert!(!verify_signature(&other_key, "1220ab", &signature)?);
        Ok(())
    }

    #[test]
    fn refuses_a_small_order_key() {
        // The identity point, y = 1, has order 1.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(VerifyingKey::from_bytes(&identity).is_ok());
        assert!(matches!(
            parse_public_key(&hex::encode(identity)),
            Err(Error::InvalidPublicKey)
        ));
    }

    /// For y below 19, y + p still fits in 255 bits, so the point has a second
    /// encoding that decompression accepts. Only the canonical one may pass.
    #[test]
    fn refuses_a_non_canonical_encoding() {
        let mut checked = 0;
        for y in 2u8..19 {
            let mut canonical = [0u8; 32];
            canonical[0] = y;
            let Ok(key) = VerifyingKey::from_bytes(&canonical) else {
                continue;
            };
            if key.is_weak() {
                continue;
            }
            // p = 2^255 - 19, little-endian: ed ff .. ff 7f.
            let mut non_canonical = [0xffu8; 32];
            non_canonical[0] = 0xed + y;
            non_canonical[31] = 0x7f;
            assert!(VerifyingKey::from_bytes(&non_canonical).is_ok(), "y = {y}");

            assert!(parse_public_key(&hex::encode(canonical)).is_ok(), "y = {y}");
            assert!(
                matches!(
                    parse_public_key(&hex::encode(non_canonical)),
                    Err(Error::InvalidPublicKey)
                ),
                "y = {y}"
            );
            checked += 1;
        }
        assert!(checked > 0, "no small y gave a valid point");
    }

    #[test]
    fn accepts_a_generated_key() {
        let key = SigningKey::from_bytes(&[7; 32]).verifying_key();
        assert_eq!(
            parse_public_key(&hex::encode(key.as_bytes())).ok(),
            Some(key)
        );
    }

    #[test]
    fn malformed_input_is_an_error() {
        let (public_key, signature) = signed(1, &[0xab]);
        assert!(matches!(
            verify_signature("zz", "ab", &signature),
            Err(Error::InvalidPublicKey)
        ));
        assert!(matches!(
            verify_signature(&public_key, "a", &signature),
            Err(Error::InvalidDigest)
        ));
        assert!(matches!(
            verify_signature(&public_key, "ab", "abcd"),
            Err(Error::InvalidSignature)
        ));
    }
}
