use std::sync::OnceLock;

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, OsRng, rand_core::RngCore},
};
use anyhow::Context;

use crate::error::Result;

/// Global encryption key, set once at startup
static ENCRYPTION_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Initialize the encryption key from a passphrase.
/// Derives a 32-byte key using SHA-256.
pub fn init_key(passphrase: &str) {
    use sha2::{Digest, Sha256};

    let key: [u8; 32] = Sha256::digest(passphrase.as_bytes()).into();
    ENCRYPTION_KEY
        .set(key)
        .expect("encryption key already initialized");
}

/// Check if encryption is enabled
pub fn is_enabled() -> bool {
    ENCRYPTION_KEY.get().is_some()
}

/// Encrypt a plaintext string. Returns base64-encoded `nonce || ciphertext`.
/// If encryption is not enabled, returns the plaintext unchanged.
pub fn encrypt(plaintext: &str) -> Result<String> {
    let Some(key) = ENCRYPTION_KEY.get() else {
        return Ok(plaintext.to_string());
    };

    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce_bytes.into(), plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &combined,
    ))
}

/// Decrypt a base64-encoded `nonce || ciphertext` string.
/// If encryption is not enabled, returns the input unchanged.
/// If decryption fails (e.g. plaintext from before encryption was enabled),
/// returns the input unchanged for backward compatibility.
pub fn decrypt(stored: &str) -> Result<String> {
    let Some(key) = ENCRYPTION_KEY.get() else {
        return Ok(stored.to_string());
    };

    let decoded = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, stored) {
        Ok(d) if d.len() > 12 => d,
        _ => return Ok(stored.to_string()), // Not encrypted, return as-is
    };

    let (nonce_bytes, ciphertext) = decoded.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    let nonce: [u8; 12] = nonce_bytes.try_into().unwrap();

    match cipher.decrypt(&nonce.into(), ciphertext) {
        Ok(plaintext) => String::from_utf8(plaintext).context("decrypted value is not valid UTF-8"),
        Err(_) => Ok(stored.to_string()), // Decryption failed, likely plaintext
    }
}

/// Encrypt an optional field
pub fn encrypt_opt(value: &Option<String>) -> Result<Option<String>> {
    match value {
        Some(v) => Ok(Some(encrypt(v)?)),
        None => Ok(None),
    }
}

/// Decrypt an optional field
pub fn decrypt_opt(value: Option<String>) -> Result<Option<String>> {
    match value {
        Some(v) => Ok(Some(decrypt(&v)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        // Use a test-only key (don't use the global OnceLock for this)
        use sha2::{Digest, Sha256};
        let key: [u8; 32] = Sha256::digest(b"test-key").into();
        ENCRYPTION_KEY.set(key).ok(); // May already be set by another test

        let plaintext = "my-secret-value";
        let encrypted = encrypt(plaintext).unwrap();
        assert_ne!(encrypted, plaintext);

        let decrypted = decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_decrypt_plaintext_passthrough() {
        // Plaintext that isn't valid base64+AES should pass through
        let result = decrypt("just-a-plain-string").unwrap();
        assert_eq!(result, "just-a-plain-string");
    }

    #[test]
    fn test_encrypt_opt_none() {
        let result = encrypt_opt(&None).unwrap();
        assert_eq!(result, None);
    }
}
