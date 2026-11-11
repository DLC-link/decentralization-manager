//! DecMan's own store for this node's Daml signing key of each party.
//!
//! Canton 3.7 refuses to export a vault key unless the node sets
//! `canton.parameters.enable-key-download`, and its admin API cannot sign
//! arbitrary bytes. DecMan therefore keeps a copy of each key it signs
//! prepared transactions with. It copies the key out of the participant while
//! the participant still allows that, and it never deletes the vault's copy.
//!
//! Each key is encrypted with AES-256-GCM. DecMan takes the encryption key
//! from the first of these sources that is set:
//!
//! 1. The file named by `DECPM_KEY_STORE_KEY_FILE`.
//! 2. A key derived from `DECPM_DB_ENCRYPTION_KEY`.
//! 3. In insecure mode only, `key-store.key` in the data directory. DecMan
//!    creates that file on first use.
//!
//! With none of them, the store is off, and DecMan signs through the KMS or
//! a fresh export as before.
//!
//! A node may copy its keys under the derived key and mount a key file
//! later. DecMan then still reads those rows with the derived key, and it
//! re-encrypts each one under the file's key when it first reads it.

use std::{
    io,
    path::{Path, PathBuf},
};

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Generate, Payload},
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use canton_proto_rs::com::digitalasset::canton::crypto::v30::{Signature, SigningPublicKey};
use ed25519_dalek::SigningKey;
use sqlx::SqlitePool;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};
use zeroize::Zeroizing;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    db::crypto,
    error::Result,
    signing::{
        PreparedTransactionHash, SigningError, SigningKeyContext, TransactionSigner,
        vault_export::{ed25519_public_key, sign_with_seed},
    },
    utils::compute_fingerprint,
};

/// Name of the encryption key file DecMan creates in insecure mode.
pub const DEFAULT_KEY_FILENAME: &str = "key-store.key";

/// Label of the key derived from the database encryption key.
const DERIVED_KEY_LABEL: &str = "decman-party-key-store-v1";

/// What `open` tells the operator when no encryption key is configured.
pub const STORE_OFF_HINT: &str = "DecMan's key store is off. Set DECPM_KEY_STORE_KEY_FILE to a \
     mounted secret, or set DECPM_DB_ENCRYPTION_KEY to at least 32 characters";

/// Domain label in the associated data, so a ciphertext from another use of
/// the same key never decrypts here.
const AAD_LABEL: &str = "decman-party-key-v1";

/// The encrypted store. Opened per use from the node config, so the
/// encryption key is read from its file and never cached in a global.
#[derive(Clone)]
pub struct KeyStore {
    db: SqlitePool,
    key: Zeroizing<[u8; 32]>,
    /// The key derived from the database encryption key, when a key file
    /// takes precedence over it. Rows written before the file was mounted
    /// are under this key.
    derived: Option<Zeroizing<[u8; 32]>>,
}

/// What the store holds for one key.
enum Row {
    Missing,
    Readable(Zeroizing<[u8; 32]>),
    /// The row decrypts under no key the store has.
    Unreadable(anyhow::Error),
}

impl KeyStore {
    /// Open the store with the configured encryption key. Returns `None`
    /// when no source of an encryption key is configured.
    ///
    /// # Errors
    ///
    /// Errors when a configured key file is missing or unreadable, or when a
    /// key file does not hold a 32-byte key.
    pub async fn open(config: &NodeConfig, db: SqlitePool) -> Result<Option<Self>> {
        let derived = crypto::derive_key(DERIVED_KEY_LABEL);
        let (key, derived) = if let Some(path) = &config.key_store_key_file {
            (read_key_file(path).await?, derived)
        } else if let Some(key) = derived {
            (key, None)
        } else if config.insecure {
            (
                read_or_create_default(&config.data_dir().join(DEFAULT_KEY_FILENAME)).await?,
                None,
            )
        } else {
            return Ok(None);
        };
        Ok(Some(Self { db, key, derived }))
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
        match self.load(party, fingerprint).await? {
            Row::Missing => Ok(None),
            Row::Readable(seed) => Ok(Some(seed)),
            Row::Unreadable(e) => Err(e),
        }
    }

    /// Read and decrypt the row of `fingerprint`'s key for `party`. A row
    /// that only the derived key decrypts is re-encrypted under the key file.
    async fn load(&self, party: &CantonId, fingerprint: &Fingerprint) -> Result<Row> {
        let Some(row) = self.read_row(party, fingerprint).await? else {
            return Ok(Row::Missing);
        };
        let error = match open_row(&self.key, party, fingerprint, &row) {
            Ok(seed) => return Ok(Row::Readable(seed)),
            Err(e) => e,
        };
        let Some(seed) = self
            .derived
            .as_ref()
            .and_then(|derived| open_row(derived, party, fingerprint, &row).ok())
        else {
            return Ok(Row::Unreadable(error));
        };
        match self.write_row(party, fingerprint, &seed, true).await {
            Ok(_) => tracing::info!(
                "Re-encrypted DecMan's copy of key {fingerprint} for {party} under the key file"
            ),
            Err(e) => tracing::warn!(
                "Could not re-encrypt DecMan's copy of key {fingerprint} for {party} under the \
                 key file: {e:#}"
            ),
        }
        Ok(Row::Readable(seed))
    }

    async fn read_row(
        &self,
        party: &CantonId,
        fingerprint: &Fingerprint,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        Ok(sqlx::query_as(
            "SELECT nonce, ciphertext FROM party_key_store WHERE party_id = ?1 AND fingerprint = ?2",
        )
        .bind(party.to_string())
        .bind(fingerprint.as_str())
        .fetch_optional(&self.db)
        .await?)
    }

    /// Store `seed` as `party`'s key `public_key`. A row that still decrypts
    /// is kept. A row that does not decrypt is replaced, because the seed is
    /// checked against the public key first. Returns whether it wrote a row.
    ///
    /// # Errors
    ///
    /// Errors when the seed does not belong to `public_key`, when `fingerprint`
    /// is not `public_key`'s, or when the write fails.
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
        anyhow::ensure!(
            compute_fingerprint(public_key) == *fingerprint,
            "{fingerprint} is not the fingerprint of that public key; not storing the seed"
        );
        let replace = match self.load(party, fingerprint).await? {
            Row::Missing => false,
            Row::Readable(_) => return Ok(false),
            Row::Unreadable(_) => {
                tracing::warn!(
                    "Replacing DecMan's unreadable copy of key {fingerprint} for {party}"
                );
                true
            }
        };
        self.write_row(party, fingerprint, seed, replace).await
    }

    /// Encrypt `seed` under the store's key and write it. `replace` updates
    /// the existing row, and otherwise an existing row is kept. Returns
    /// whether it wrote a row.
    async fn write_row(
        &self,
        party: &CantonId,
        fingerprint: &Fingerprint,
        seed: &[u8; 32],
        replace: bool,
    ) -> Result<bool> {
        let nonce: [u8; 12] = Generate::try_generate()
            .map_err(|e| anyhow::anyhow!("nonce generation failed: {e}"))?;
        let ciphertext = cipher(&self.key)
            .encrypt(
                &nonce.into(),
                Payload {
                    msg: seed,
                    aad: &associated_data(party, fingerprint),
                },
            )
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;
        let stored_at = chrono::Utc::now().timestamp();
        let statement = if replace {
            "UPDATE party_key_store SET nonce = ?3, ciphertext = ?4, stored_at = ?5 \
             WHERE party_id = ?1 AND fingerprint = ?2"
        } else {
            "INSERT INTO party_key_store (party_id, fingerprint, nonce, ciphertext, stored_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (party_id, fingerprint) DO NOTHING"
        };
        let result = sqlx::query(statement)
            .bind(party.to_string())
            .bind(fingerprint.as_str())
            .bind(nonce.as_slice())
            .bind(ciphertext)
            .bind(stored_at)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() == 1)
    }
}

fn cipher(key: &[u8; 32]) -> Aes256Gcm {
    Aes256Gcm::new(key.into())
}

/// Decrypt a row with `key`.
fn open_row(
    key: &[u8; 32],
    party: &CantonId,
    fingerprint: &Fingerprint,
    (nonce, ciphertext): &(Vec<u8>, Vec<u8>),
) -> Result<Zeroizing<[u8; 32]>> {
    let nonce: [u8; 12] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("Stored key {fingerprint} has a malformed nonce"))?;
    let plaintext = Zeroizing::new(
        cipher(key)
            .decrypt(
                &nonce.into(),
                Payload {
                    msg: ciphertext,
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
    Ok(Zeroizing::new(seed))
}

fn associated_data(party: &CantonId, fingerprint: &Fingerprint) -> Vec<u8> {
    format!("{AAD_LABEL}\0{party}\0{fingerprint}").into_bytes()
}

/// Read an encryption key file: 64 hex characters, or the base64 of 32
/// bytes.
async fn read_key_file(path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    let contents = Zeroizing::new(tokio::fs::read(path).await.map_err(|e| {
        anyhow::anyhow!(
            "Cannot read the key-store encryption key at {path}: {e}",
            path = path.display()
        )
    })?);
    parse_key(&contents).ok_or_else(|| {
        anyhow::anyhow!(
            "{path} holds no 32-byte key: expected 64 hex characters or the base64 of 32 bytes",
            path = path.display()
        )
    })
}

/// Raw bytes are refused, so a 32-character passphrase never becomes a key.
fn parse_key(contents: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
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
    if let Err(e) = write_private(path, encoded.as_bytes()).await {
        // Another process created the file first, so use its key.
        if e.downcast_ref::<io::Error>().map(io::Error::kind) == Some(io::ErrorKind::AlreadyExists)
        {
            return read_key_file(path).await;
        }
        return Err(e);
    }
    tracing::warn!(
        path = %path.display(),
        "Created the key-store encryption key next to the database. Set \
         DECPM_KEY_STORE_KEY_FILE to a mounted secret to keep the two apart"
    );
    Ok(key)
}

/// Create `path` readable by this user only. The contents go to a temporary
/// file first and are linked into place whole, so a crash never leaves a
/// partial key. Fails if `path` exists, so two processes never write
/// different keys.
async fn write_private(path: &Path, contents: &[u8]) -> Result {
    let suffix: [u8; 8] = Generate::try_generate()
        .map_err(|e| anyhow::anyhow!("temporary name generation failed: {e}"))?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".tmp-{}", hex::encode(suffix)));
    let temporary = PathBuf::from(temporary);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let written = async {
        let mut file = options.open(&temporary).await?;
        file.write_all(contents).await?;
        file.sync_all().await?;
        tokio::fs::hard_link(&temporary, path).await
    }
    .await;
    // The link holds the contents now, or the write failed. Either way the
    // temporary name has no further use.
    let _ = tokio::fs::remove_file(&temporary).await;
    Ok(written?)
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        CryptoKeyFormat, SigningKeySpec,
    };
    use ed25519_dalek::{Signature as DalekSignature, Verifier};

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
            derived: None,
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
    async fn a_readable_stored_key_is_never_replaced(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        let store = store(db, 1);

        assert!(store.put(&party(1), &fingerprint, &public, &seed).await?);
        assert!(!store.put(&party(1), &fingerprint, &public, &seed).await?);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_row_under_an_old_encryption_key_is_replaced(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        store(db.clone(), 1)
            .put(&party(1), &fingerprint, &public, &seed)
            .await?;

        let new_store = store(db.clone(), 2);
        assert!(new_store.get(&party(1), &fingerprint).await.is_err());
        assert!(
            new_store
                .put(&party(1), &fingerprint, &public, &seed)
                .await?
        );
        assert_eq!(
            new_store.get(&party(1), &fingerprint).await?.as_deref(),
            Some(&seed)
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM party_key_store")
            .fetch_one(&db)
            .await?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_row_under_the_derived_key_moves_to_the_key_file(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        store(db.clone(), 1)
            .put(&party(1), &fingerprint, &public, &seed)
            .await?;

        let with_file = KeyStore {
            db: db.clone(),
            key: Zeroizing::new([2; 32]),
            derived: Some(Zeroizing::new([1; 32])),
        };
        assert_eq!(
            with_file.get(&party(1), &fingerprint).await?.as_deref(),
            Some(&seed)
        );
        assert!(
            !with_file
                .put(&party(1), &fingerprint, &public, &seed)
                .await?
        );

        // The file's key alone now reads the row.
        assert_eq!(
            store(db, 2).get(&party(1), &fingerprint).await?.as_deref(),
            Some(&seed)
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_row_under_neither_key_stays_unreadable(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let fingerprint = utils::compute_fingerprint(&public);
        store(db.clone(), 1)
            .put(&party(1), &fingerprint, &public, &seed)
            .await?;

        let with_file = KeyStore {
            db: db.clone(),
            key: Zeroizing::new([2; 32]),
            derived: Some(Zeroizing::new([3; 32])),
        };
        assert!(with_file.get(&party(1), &fingerprint).await.is_err());
        assert!(store(db, 1).get(&party(1), &fingerprint).await?.is_some());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_seed_under_another_fingerprint_is_refused(db: SqlitePool) -> Result {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let other = utils::compute_fingerprint(&public_key(&[8u8; 32]));
        assert!(
            store(db, 1)
                .put(&party(1), &other, &public, &seed)
                .await
                .is_err()
        );
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
    fn a_key_file_holds_hex_or_base64() {
        let key = [9u8; 32];
        for contents in [
            format!("{}\n", hex::encode(key)).into_bytes(),
            STANDARD.encode(key).into_bytes(),
        ] {
            assert_eq!(parse_key(&contents).as_deref(), Some(&key));
        }
        assert!(parse_key(b"too short").is_none());
        assert!(parse_key(STANDARD.encode([9u8; 16]).as_bytes()).is_none());
    }

    #[test]
    fn a_32_byte_passphrase_is_not_a_key() {
        assert!(parse_key(b"correct horse battery staple 123").is_none());
        assert!(parse_key(&[9u8; 32]).is_none());
    }

    #[tokio::test]
    async fn the_default_key_file_is_created_once_and_read_back() -> Result {
        let dir = std::env::temp_dir().join(format!("decman-key-store-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(DEFAULT_KEY_FILENAME);
        let created = read_or_create_default(&path).await?;
        let read = read_or_create_default(&path).await?;
        assert_eq!(*created, *read);
        let mut names = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            names.push(entry.file_name());
        }
        assert_eq!(names, [DEFAULT_KEY_FILENAME]);
        #[cfg(unix)]
        {
            let mode = tokio::fs::metadata(&path).await?.permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        tokio::fs::remove_dir_all(&dir).await?;
        Ok(())
    }
}
