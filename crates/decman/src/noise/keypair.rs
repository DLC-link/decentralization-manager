#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs::Permissions, path::Path};

use anyhow::Context;
use secp256k1::{PublicKey, Secp256k1, SecretKey, ecdh::SharedSecret};
use tokio::fs::OpenOptions;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

use super::NoiseError;
use crate::error::Result;

/// Parse a hex-encoded public key string into a PublicKey
pub fn parse_public_key(hex_str: &str) -> Result<PublicKey, NoiseError> {
    let pub_key_bytes = hex::decode(hex_str).map_err(|_| NoiseError::InvalidMessage)?;
    let pub_key = PublicKey::from_slice(&pub_key_bytes).map_err(|_| NoiseError::InvalidMessage)?;
    Ok(pub_key)
}

/// Owned, zeroizing secp256k1 secret-key bytes.
///
/// Validated against `SecretKey::from_slice` on construction so subsequent
/// conversions back to a `SecretKey` cannot fail. The inner bytes live in
/// `Zeroizing<[u8; 32]>` and are wiped on drop. Intentionally no `Clone`,
/// `Copy`, `Debug`, or `Serialize` — secret access goes through the methods
/// on `NoiseKeypair`.
pub struct NoiseSecretKey(Zeroizing<[u8; 32]>);

impl NoiseSecretKey {
    fn from_bytes(bytes: [u8; 32]) -> Result<Self> {
        // Validate via secp256k1; the constructed SecretKey is dropped here.
        SecretKey::from_slice(&bytes)?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    fn to_secp_secret_key(&self) -> SecretKey {
        // Bytes were validated on construction; reconstruction is infallible.
        SecretKey::from_slice(&self.0[..])
            .expect("NoiseSecretKey bytes were validated on construction")
    }

    fn as_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(&self.0[..]))
    }
}

/// Static keypair for Noise protocol authentication
pub struct NoiseKeypair {
    secret_key: NoiseSecretKey,
    pub public_key: PublicKey,
}

impl NoiseKeypair {
    /// Generate a new random keypair
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut secp256k1::rand::thread_rng());
        let bytes = secret_key.secret_bytes();
        Self {
            secret_key: NoiseSecretKey::from_bytes(bytes)
                .expect("freshly-generated secp256k1 secret key is always valid"),
            public_key,
        }
    }

    /// Load keypair from a file (expects hex-encoded secret key)
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        // Tighten permissions BEFORE reading so any existing overly-permissive
        // file is locked down before we leak its bytes onto our heap. Also
        // runs here (not just in `load_or_generate_keypair`) so every other
        // caller — workflow handlers, noise/client.rs, noise/server.rs — picks
        // up the chmod even on the direct `from_file` path.
        #[cfg(unix)]
        ensure_key_file_permissions(path).await?;
        // Read as raw bytes into a zeroizing buffer so the hex representation
        // never lives in a non-zeroizing `String`. The decoded 32-byte secret
        // is also held in a zeroizing buffer before being moved into
        // `NoiseSecretKey`.
        let raw =
            Zeroizing::new(tokio::fs::read(path).await.with_context(|| {
                format!("Failed to read key file '{path}'", path = path.display())
            })?);
        let hex_str = std::str::from_utf8(&raw)
            .with_context(|| format!("Key file '{path}' is not UTF-8", path = path.display()))?
            .trim();
        let decoded = Zeroizing::new(hex::decode(hex_str)?);
        let bytes: [u8; 32] = decoded.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "Key file '{path}' did not contain 32 bytes of hex",
                path = path.display()
            )
        })?;
        let secret_key = NoiseSecretKey::from_bytes(bytes)?;
        let secp = Secp256k1::new();
        let public_key = PublicKey::from_secret_key(&secp, &secret_key.to_secp_secret_key());
        Ok(Self {
            secret_key,
            public_key,
        })
    }

    /// Save the private key to a file (hex-encoded)
    ///
    /// On unix, the file is created with mode 0600 atomically — no window
    /// in which the file is readable to other users.
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result {
        let path = path.as_ref();

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "Failed to create directory '{parent}'",
                    parent = parent.display()
                )
            })?;
        }

        let hex_string = self.secret_key.as_hex();

        #[cfg(unix)]
        {
            // `OpenOptions::mode` only applies on create — when the target
            // file already exists, its prior permissions are preserved.
            // Tighten to 0600 first so the truncate-and-write below cannot
            // leave the new key bytes briefly readable to other users.
            if path.exists() {
                tokio::fs::set_permissions(path, Permissions::from_mode(0o600))
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to chmod existing key file '{path}'",
                            path = path.display()
                        )
                    })?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .await
                .with_context(|| {
                    format!("Failed to open key file '{path}'", path = path.display())
                })?;
            file.write_all(hex_string.as_bytes())
                .await
                .with_context(|| {
                    format!("Failed to write key file '{path}'", path = path.display())
                })?;
            file.sync_all().await.with_context(|| {
                format!("Failed to sync key file '{path}'", path = path.display())
            })?;
        }
        #[cfg(not(unix))]
        {
            tokio::fs::write(path, hex_string.as_bytes())
                .await
                .with_context(|| {
                    format!("Failed to write key file '{path}'", path = path.display())
                })?;
        }

        Ok(())
    }

    /// Get the public key as hex string
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key.serialize())
    }

    /// Derive a pre-shared key (PSK) from a peer's public key using ECDH
    pub fn derive_psk(&self, peer_public_key: &PublicKey) -> Zeroizing<[u8; 32]> {
        let secp_sk = self.secret_key.to_secp_secret_key();
        Zeroizing::new(SharedSecret::new(peer_public_key, &secp_sk).secret_bytes())
    }
}

/// Ensure the key file has restrictive (0600) permissions.
///
/// Idempotent: runs on every load so existing keys deployed with a more
/// permissive default are tightened on the next startup. Emits one
/// `warn!` when it has to change anything, so operators can confirm
/// which nodes had the pre-fix permissions.
#[cfg(unix)]
async fn ensure_key_file_permissions(path: &Path) -> Result {
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("Failed to stat key file '{path}'", path = path.display()))?;
    let current_mode = metadata.permissions().mode() & 0o777;
    if current_mode != 0o600 {
        tracing::warn!(
            "Tightening permissions on key file {path} from {current_mode:o} to 0600",
            path = path.display(),
        );
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(path, perms)
            .await
            .with_context(|| format!("Failed to chmod key file '{path}'", path = path.display()))?;
    }
    Ok(())
}

/// Load or generate a Noise keypair
///
/// If the key file exists, loads it. Otherwise, generates a new keypair and saves it.
/// This is the primary way to obtain a keypair for the application.
pub async fn load_or_generate_keypair<P: AsRef<Path>>(path: P) -> Result<NoiseKeypair> {
    let path = path.as_ref();

    if path.exists() {
        tracing::debug!(
            "Loading existing Noise keypair from {path}",
            path = path.display()
        );
        // `from_file` itself calls `ensure_key_file_permissions` first, so
        // every caller (not just this helper) gets the chmod-on-load.
        NoiseKeypair::from_file(path).await
    } else {
        tracing::info!("No Noise keypair found, generating new one");
        let keypair = NoiseKeypair::generate();
        keypair.save_to_file(path).await?;
        tracing::info!("Noise keypair saved to {path}", path = path.display());
        tracing::info!("Public key: {key}", key = keypair.public_key_hex());
        Ok(keypair)
    }
}

#[cfg(test)]
mod tests {
    use std::{fmt::Debug, fs::Permissions};

    use super::*;

    // Compile-time guard: the secret-bearing types must not expose Debug,
    // Clone, or Copy. Removing any of these breaks the security invariant.
    static_assertions::assert_not_impl_any!(NoiseKeypair: Debug, Clone, Copy);
    static_assertions::assert_not_impl_any!(NoiseSecretKey: Debug, Clone, Copy);

    #[cfg(unix)]
    #[tokio::test]
    async fn save_to_file_creates_0600() -> Result {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("noise.key");

        let keypair = NoiseKeypair::generate();
        keypair.save_to_file(&path).await?;

        let mode = tokio::fs::metadata(&path).await?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_to_file_overwrites_existing_with_0600() -> Result {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("noise.key");

        // Pre-create the file with 0644 permissions and stale contents
        tokio::fs::write(&path, b"stale-key-material").await?;
        tokio::fs::set_permissions(&path, Permissions::from_mode(0o644)).await?;
        assert_eq!(
            tokio::fs::metadata(&path).await?.permissions().mode() & 0o777,
            0o644,
        );

        let keypair = NoiseKeypair::generate();
        keypair.save_to_file(&path).await?;

        let mode = tokio::fs::metadata(&path).await?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        let written = tokio::fs::read_to_string(&path).await?;
        assert_eq!(written.len(), 64, "expected 32-byte hex string");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_or_generate_keypair_tightens_permissions() -> Result {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("noise.key");

        // Seed a valid key file, then loosen perms to 0644
        let initial = NoiseKeypair::generate();
        initial.save_to_file(&path).await?;
        tokio::fs::set_permissions(&path, Permissions::from_mode(0o644)).await?;
        assert_eq!(
            tokio::fs::metadata(&path).await?.permissions().mode() & 0o777,
            0o644,
        );

        let loaded = load_or_generate_keypair(&path).await?;
        assert_eq!(loaded.public_key_hex(), initial.public_key_hex());

        let mode = tokio::fs::metadata(&path).await?.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "expected chmod-on-load to tighten to 0600, got {mode:o}"
        );
        Ok(())
    }
}
