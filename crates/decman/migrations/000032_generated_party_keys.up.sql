-- Daml signing keys that DecMan generated itself, on a node that keeps its
-- keys in Canton's software vault. Canton never holds these keys, so it never
-- has to export one. A key's name follows the vault's naming of party Daml
-- keys, so a retried run finds its key again. Each seed is encrypted like a
-- row of party_key_store, and the associated data binds it to its name and
-- fingerprint.
CREATE TABLE generated_party_keys (
    name        TEXT PRIMARY KEY NOT NULL,
    fingerprint TEXT NOT NULL UNIQUE,
    public_key  BLOB NOT NULL,
    nonce       BLOB NOT NULL,
    ciphertext  BLOB NOT NULL,
    created_at  INTEGER NOT NULL
);
