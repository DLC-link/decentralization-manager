//! DecMan's own store for this node's Daml signing key of each party.
//!
//! Canton 3.7 refuses to export a vault key unless the node sets
//! `canton.parameters.enable-key-download`, and its admin API cannot sign
//! arbitrary bytes. DecMan therefore keeps a copy of each key it signs
//! prepared transactions with. It copies the key out of the participant while
//! the participant still allows that, and it never deletes the vault's copy.
//!
//! Each key is encrypted with AES-256-GCM. The encryption key comes from a
//! file outside the database: the path in `DECPM_KEY_STORE_KEY_FILE`, or
//! `key-store.key` in the data directory, which DecMan creates on first use.
//! The second is a stopgap: that file sits next to the database it protects.

use std::path::{Path, PathBuf};

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Generate, Payload},
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use canton_proto_rs::com::digitalasset::canton::crypto::v30::{Signature, SigningPublicKey};
use ed25519_dalek::SigningKey;
use sqlx::SqlitePool;
use zeroize::Zeroizing;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    signing::{
        PreparedTransactionHash, SigningError, SigningKeyContext, TransactionSigner,
        vault_export::{ed25519_public_key, sign_with_seed},
    },
};

/// Name of the encryption key file DecMan creates when none is configured.
pub const DEFAULT_KEY_FILENAME: &str = "key-store.key";

/// Domain label in the associated data, so a ciphertext from another use of
/// the same key never decrypts here.
const AAD_LABEL: &str = "decman-party-key-v1";

/// The encrypted store. Opened per use from the node config, so the
/// encryption key is read from its file and never cached in a global.
#[derive(Clone)]
pub struct KeyStore {
    db: SqlitePool,
    key: Zeroizing<[u8; 32]>,
}

impl KeyStore {
    /// Open the store with the configured encryption key, creating the
    /// default key file when no path is configured and none exists yet.
    ///
    /// # Errors
    ///
    /// Errors when a configured key file is missing or unreadable, or when a
    /// key file does not hold a 32-byte key.
    pub async fn open(config: &NodeConfig, db: SqlitePool) -> Result<Self> {
        let key = match &config.key_store_key_file {
            Some(path) => read_key_file(path).await?,
            None => read_or_create_default(&config.data_dir().join(DEFAULT_KEY_FILENAME)).await?,
        };
        Ok(Self { db, key })
    }

    /// The stored seed of `fingerprint`'s key for `party`, if DecMan holds it.
    ///
    /// # Errors
    ///
    /// Errors when the row does not decrypt, which means the encryption key
    /// changed or the row was tampered with.
    pub async fn get(
        &self,
        party: &CantonId,
        fingerprint: &Fingerprint,
    ) -> Result<Option<Zeroizing<[u8; 32]>>> {
        let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT nonce, ciphertext FROM party_key_store WHERE party_id = ?1 AND fingerprint = ?2",
        )
        .bind(party.to_string())
        .bind(fingerprint.as_str())
        .fetch_optional(&self.db)
        .await?;
        let Some((nonce, ciphertext)) = row else {
            return Ok(None);
        };
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_| anyhow::anyhow!("Stored key {fingerprint} has a malformed nonce"))?;
        let plaintext = Zeroizing::new(
            self.cipher()
                .decrypt(
                    &nonce.into(),
                    Payload {
                        msg: &ciphertext,
                        aad: &associated_data(party, fingerprint),
                    },
                )
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Stored key {fingerprint} for {party} does not decrypt. The key-store \
                         encryption key changed, or the row was altered"
                    )
                })?,
        );
        let seed: [u8; 32] = plaintext
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Stored key {fingerprint} is not a 32-byte seed"))?;
        Ok(Some(Zeroizing::new(seed)))
    }

    /// Store `seed` as `party`'s key `public_key`, unless DecMan already holds
    /// a key with that fingerprint. Returns whether it stored the seed.
    ///
    /// # Errors
    ///
    /// Errors when the seed does not belong to `public_key`, or the write fails.
    pub async fn put(
        &self,
        party: &CantonId,
        fingerprint: &Fingerprint,
        public_key: &SigningPublicKey,
        seed: &[u8; 32],
    ) -> Result<bool> {
        let expected = ed25519_public_key(public_key)?;
        anyhow::ensure!(
            SigningKey::from_bytes(seed).verifying_key().to_bytes() == expected,
            "The seed for {fingerprint} does not produce that public key; not storing it"
        );
        let nonce: [u8; 12] = Generate::try_generate()
            .map_err(|e| anyhow::anyhow!("nonce generation failed: {e}"))?;
        let ciphertext = self
            .cipher()
            .encrypt(
                &nonce.into(),
                Payload {
                    msg: seed,
                    aad: &associated_data(party, fingerprint),
                },
            )
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;
        let stored_at = chrono::Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT INTO party_key_store (party_id, fingerprint, nonce, ciphertext, stored_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (party_id, fingerprint) DO NOTHING",
        )
        .bind(party.to_string())
        .bind(fingerprint.as_str())
        .bind(nonce.as_slice())
        .bind(ciphertext)
        .bind(stored_at)
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    fn cipher(&self) -> Aes256Gcm {
        let key: &[u8; 32] = &self.key;
        Aes256Gcm::new(key.into())
    }
}

fn associated_data(party: &CantonId, fingerprint: &Fingerprint) -> Vec<u8> {
    format!("{AAD_LABEL}\0{party}\0{fingerprint}").into_bytes()
}

/// Read an encryption key file: 32 raw bytes, 64 hex characters, or the
/// base64 of 32 bytes.
async fn read_key_file(path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    let contents = Zeroizing::new(tokio::fs::read(path).await.map_err(|e| {
        anyhow::anyhow!(
            "Cannot read the key-store encryption key at {path}: {e}",
            path = path.display()
        )
    })?);
    parse_key(&contents).ok_or_else(|| {
        anyhow::anyhow!(
            "{path} holds no 32-byte key: expected 32 raw bytes, 64 hex characters or base64",
            path = path.display()
        )
    })
}

fn parse_key(contents: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    if let Ok(raw) = <[u8; 32]>::try_from(contents) {
        return Some(Zeroizing::new(raw));
    }
    let text = std::str::from_utf8(contents).ok()?.trim();
    let decoded = Zeroizing::new(
        hex::decode(text)
            .ok()
            .filter(|bytes| bytes.len() == 32)
            .or_else(|| STANDARD.decode(text).ok())?,
    );
    <[u8; 32]>::try_from(decoded.as_slice())
        .ok()
        .map(Zeroizing::new)
}

async fn read_or_create_default(path: &PathBuf) -> Result<Zeroizing<[u8; 32]>> {
    if tokio::fs::try_exists(path).await? {
        return read_key_file(path).await;
    }
    let key = Zeroizing::new(
        <[u8; 32]>::try_generate().map_err(|e| anyhow::anyhow!("key generation failed: {e}"))?,
    );
    let encoded = Zeroizing::new(STANDARD.encode(key.as_slice()));
    write_private(path, encoded.as_bytes()).await?;
    tracing::warn!(
        path = %path.display(),
        "Created the key-store encryption key next to the database. Set \
         DECPM_KEY_STORE_KEY_FILE to a mounted secret to keep the two apart"
    );
    Ok(key)
}

/// Create `path` readable by this user only. Fails if the file exists, so two
/// processes never write different keys.
async fn write_private(path: &Path, contents: &[u8]) -> Result {
    use tokio::io::AsyncWriteExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    file.write_all(contents).await?;
    file.sync_all().await?;
    Ok(())
}

/// Signs with a key from DecMan's store.
pub struct StoredKeySigner {
    seed: Zeroizing<[u8; 32]>,
}

impl StoredKeySigner {
    pub fn new(seed: Zeroizing<[u8; 32]>) -> Self {
        Self { seed }
    }
}

#[async_trait]
impl TransactionSigner for StoredKeySigner {
    async fn sign(
        &self,
        hashes: &[PreparedTransactionHash],
        key: &SigningKeyContext,
    ) -> Result<Vec<Signature>, SigningError> {
        tracing::info!("Signing with the key from DecMan's key store");
        sign_with_seed(&self.seed, hashes, key)
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        CryptoKeyFormat, SigningKeySpec,
    };

    use super::*;
    use crate::{
        canton_id::{NAMESPACE_LENGTH, Namespace},
        db::MIGRATOR,
        utils,
    };

    /// DER prefix of an Ed25519 `SubjectPublicKeyInfo`.
    const SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];

    fn public_key(seed: &[u8; 32]) -> SigningPublicKey {
        let mut der = SPKI_PREFIX.to_vec();
        der.extend_from_slice(&SigningKey::from_bytes(seed).verifying_key().to_bytes());
        SigningPublicKey {
            format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
            public_key: der,
            key_spec: SigningKeySpec::EcCurve25519 as i32,
            ..Default::default()
        }
    }

    fn party(n: u8) -> CantonId {
        CantonId::new(format!("dec{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
    }

    fn store(db: SqlitePool, key: u8) -> KeyStore {
        KeyStore {
            db,
            key: Zeroizing::new([key; 32]),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_stored_key_comes_back_only_for_its_party(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        let store = store(db, 1);

        assert!(store.put(&party(1), &fingerprint, &public, &seed).await?);
        assert_eq!(
            store.get(&party(1), &fingerprint).await?.as_deref(),
            Some(&seed)
        );
        assert!(store.get(&party(2), &fingerprint).await?.is_none());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_stored_key_is_never_replaced(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        let store = store(db, 1);

        assert!(store.put(&party(1), &fingerprint, &public, &seed).await?);
        assert!(!store.put(&party(1), &fingerprint, &public, &seed).await?);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_seed_for_another_public_key_is_refused(db: SqlitePool) -> Result {
        let public = public_key(&[7u8; 32]);
        let fingerprint = utils::compute_fingerprint(&public);
        assert!(
            store(db, 1)
                .put(&party(1), &fingerprint, &public, &[8u8; 32])
                .await
                .is_err()
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn another_encryption_key_cannot_read_the_store(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        store(db.clone(), 1)
            .put(&party(1), &fingerprint, &public, &seed)
            .await?;
        assert!(store(db, 2).get(&party(1), &fingerprint).await.is_err());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_row_moved_to_another_party_does_not_decrypt(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        let store = store(db.clone(), 1);
        store.put(&party(1), &fingerprint, &public, &seed).await?;
        sqlx::query("UPDATE party_key_store SET party_id = ?1")
            .bind(party(2).to_string())
            .execute(&db)
            .await?;
        assert!(store.get(&party(2), &fingerprint).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn the_stored_key_signer_signs_with_the_stored_seed() -> Result {
        use ed25519_dalek::{Signature as DalekSignature, Verifier};

        let seed = [7u8; 32];
        let public = public_key(&seed);
        let key = SigningKeyContext {
            fingerprint: utils::compute_fingerprint(&public),
            public_key: public,
            kms_key_id: None,
        };
        let hash = PreparedTransactionHash::new(vec![5; 32]);
        let signatures = StoredKeySigner::new(Zeroizing::new(seed))
            .sign(std::slice::from_ref(&hash), &key)
            .await?;
        let bytes: [u8; 64] = signatures[0]
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("not a 64-byte signature"))?;
        SigningKey::from_bytes(&seed)
            .verifying_key()
            .verify(hash.as_bytes(), &DalekSignature::from_bytes(&bytes))?;
        assert_eq!(signatures[0].signed_by, key.fingerprint.as_str());
        Ok(())
    }

    #[test]
    fn a_key_file_may_be_raw_hex_or_base64() {
        let key = [9u8; 32];
        for contents in [
            key.to_vec(),
            format!("{}\n", hex::encode(key)).into_bytes(),
            STANDARD.encode(key).into_bytes(),
        ] {
            assert_eq!(parse_key(&contents).as_deref(), Some(&key));
        }
        assert!(parse_key(b"too short").is_none());
        assert!(parse_key(STANDARD.encode([9u8; 16]).as_bytes()).is_none());
    }

    #[tokio::test]
    async fn the_default_key_file_is_created_once_and_read_back() -> Result {
        let dir = std::env::temp_dir().join(format!("decman-key-store-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(DEFAULT_KEY_FILENAME);
        let created = read_or_create_default(&path).await?;
        let read = read_or_create_default(&path).await?;
        assert_eq!(*created, *read);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(&path).await?.permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        tokio::fs::remove_dir_all(&dir).await?;
        Ok(())
    }
}
