use ed25519_dalek::{Signature, VerifyingKey};

use crate::Error;

/// Parses a hex-encoded raw 32-byte Ed25519 public key.
pub fn parse_public_key(public_key_hex: &str) -> Result<VerifyingKey, Error> {
    let bytes: [u8; 32] = hex::decode(public_key_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Error::InvalidPublicKey)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| Error::InvalidPublicKey)
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
