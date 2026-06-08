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

/// Encrypt a plaintext string with an explicit key. Returns base64-encoded
/// `nonce || ciphertext`. Split out from [`encrypt`] so the cipher can be
/// exercised in tests without touching the process-global [`ENCRYPTION_KEY`].
fn encrypt_str_with_key(key: &[u8; 32], plaintext: &str) -> Result<String> {
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

/// Decrypt a base64-encoded `nonce || ciphertext` string with an explicit key.
/// Returns the input unchanged when it does not decode as `nonce || ciphertext`
/// or when AES-GCM authentication fails (legacy plaintext / wrong-key
/// compatibility).
fn decrypt_str_with_key(key: &[u8; 32], stored: &str) -> Result<String> {
    let decoded = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, stored) {
        Ok(d) if d.len() > 12 => d,
        _ => return Ok(stored.to_string()), // Not encrypted, return as-is
    };

    let (nonce_bytes, ciphertext) = decoded.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    let nonce: [u8; 12] = match nonce_bytes.try_into() {
        Ok(n) => n,
        Err(_) => return Ok(stored.to_string()),
    };

    match cipher.decrypt(&nonce.into(), ciphertext) {
        Ok(plaintext) => String::from_utf8(plaintext).context("decrypted value is not valid UTF-8"),
        Err(_) => Ok(stored.to_string()), // Decryption failed, likely plaintext
    }
}

/// Encrypt a plaintext string. Returns base64-encoded `nonce || ciphertext`.
/// If encryption is not enabled, returns the plaintext unchanged.
pub fn encrypt(plaintext: &str) -> Result<String> {
    match ENCRYPTION_KEY.get() {
        Some(key) => encrypt_str_with_key(key, plaintext),
        None => Ok(plaintext.to_string()),
    }
}

/// Decrypt a base64-encoded `nonce || ciphertext` string.
/// If encryption is not enabled, returns the input unchanged.
/// If decryption fails (e.g. plaintext from before encryption was enabled),
/// returns the input unchanged for backward compatibility.
pub fn decrypt(stored: &str) -> Result<String> {
    match ENCRYPTION_KEY.get() {
        Some(key) => decrypt_str_with_key(key, stored),
        None => Ok(stored.to_string()),
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

/// Encrypt a binary blob. Returns the raw byte form `nonce || ciphertext` (no
/// base64) suitable for a SQLite BLOB column. If encryption is not enabled,
/// returns the input unchanged so the read path can transparently round-trip
/// either form.
///
/// # Errors
///
/// Returns an error if AES-GCM encryption fails.
pub fn encrypt_bytes(plaintext: &[u8]) -> Result<Vec<u8>> {
    match ENCRYPTION_KEY.get() {
        Some(key) => encrypt_bytes_with_key(key, plaintext),
        None => Ok(plaintext.to_vec()),
    }
}

/// Encrypt a binary blob with an explicit key. Split out from [`encrypt_bytes`]
/// so the cipher can be exercised in tests without the process-global key.
fn encrypt_bytes_with_key(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce_bytes.into(), plaintext)
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);
    Ok(combined)
}

/// Decrypt a `nonce || ciphertext` blob. If encryption is not enabled, or the
/// blob is too short / decryption fails (e.g. legacy plaintext written before
/// encryption was turned on), returns the input bytes unchanged.
///
/// # Errors
///
/// Returns an error only if the stored blob has a malformed nonce length.
pub fn decrypt_bytes(stored: &[u8]) -> Result<Vec<u8>> {
    match ENCRYPTION_KEY.get() {
        Some(key) => decrypt_bytes_with_key(key, stored),
        None => Ok(stored.to_vec()),
    }
}

/// Decrypt a `nonce || ciphertext` blob with an explicit key. Returns the input
/// unchanged for short blobs or on AES-GCM failure (legacy plaintext /
/// wrong-key compatibility).
fn decrypt_bytes_with_key(key: &[u8; 32], stored: &[u8]) -> Result<Vec<u8>> {
    if stored.len() <= 12 {
        return Ok(stored.to_vec());
    }

    let (nonce_bytes, ciphertext) = stored.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    let nonce: [u8; 12] = nonce_bytes
        .try_into()
        .context("nonce slice is not 12 bytes")?;

    match cipher.decrypt(&nonce.into(), ciphertext) {
        Ok(plaintext) => Ok(plaintext),
        Err(_) => Ok(stored.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test keys are passed explicitly to the `*_with_key` cores so these tests
    // never touch the process-global `ENCRYPTION_KEY` (whose `OnceLock` made
    // the previous round-trip test order-dependent across the whole binary).
    const TEST_KEY: [u8; 32] = [7u8; 32];
    const OTHER_KEY: [u8; 32] = [9u8; 32];

    #[test]
    fn string_round_trips_and_ciphertext_differs() -> Result {
        let plaintext = "my-secret-value";
        let encrypted = encrypt_str_with_key(&TEST_KEY, plaintext)?;
        assert_ne!(
            encrypted, plaintext,
            "ciphertext must differ from plaintext"
        );
        assert_eq!(decrypt_str_with_key(&TEST_KEY, &encrypted)?, plaintext);
        Ok(())
    }

    #[test]
    fn string_decrypt_with_wrong_key_does_not_yield_plaintext() -> Result {
        // AES-GCM authentication failure falls back to returning the stored
        // value verbatim (legacy-plaintext compatibility). The security-
        // relevant property: a wrong key never reveals the original plaintext.
        let plaintext = "my-secret-value";
        let encrypted = encrypt_str_with_key(&TEST_KEY, plaintext)?;
        let out = decrypt_str_with_key(&OTHER_KEY, &encrypted)?;
        assert_ne!(out, plaintext);
        assert_eq!(out, encrypted);
        Ok(())
    }

    #[test]
    fn bytes_round_trip_and_ciphertext_differs() -> Result {
        let plaintext: &[u8] = &[0u8, 1, 2, 255, 128, 64, 7];
        let encrypted = encrypt_bytes_with_key(&TEST_KEY, plaintext)?;
        assert_ne!(encrypted.as_slice(), plaintext);
        assert_eq!(
            decrypt_bytes_with_key(&TEST_KEY, &encrypted)?,
            plaintext.to_vec()
        );
        Ok(())
    }

    #[test]
    fn bytes_short_blob_passes_through() -> Result {
        // A blob too short to carry a 12-byte nonce is returned unchanged.
        let short: &[u8] = &[1, 2, 3];
        assert_eq!(decrypt_bytes_with_key(&TEST_KEY, short)?, short.to_vec());
        Ok(())
    }

    #[test]
    fn bytes_decrypt_with_wrong_key_does_not_yield_plaintext() -> Result {
        let plaintext: &[u8] = b"binary-secret-payload-1234567890";
        let encrypted = encrypt_bytes_with_key(&TEST_KEY, plaintext)?;
        let out = decrypt_bytes_with_key(&OTHER_KEY, &encrypted)?;
        assert_ne!(out.as_slice(), plaintext);
        assert_eq!(out, encrypted);
        Ok(())
    }

    #[test]
    fn decrypt_passes_through_plaintext_when_key_unset() -> Result {
        // Public path with no global key: a value that isn't `nonce||ciphertext`
        // base64 is returned unchanged.
        assert_eq!(decrypt("just-a-plain-string")?, "just-a-plain-string");
        Ok(())
    }

    #[test]
    fn encrypt_opt_none_is_none() -> Result {
        assert_eq!(encrypt_opt(&None)?, None);
        Ok(())
    }
}
