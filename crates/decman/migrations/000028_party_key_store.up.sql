-- DecMan's own copy of this node's Daml signing key for each party.
--
-- Canton 3.7 no longer exports a vault key by default, and its admin API
-- cannot sign arbitrary bytes. DecMan therefore holds the key it signs
-- prepared transactions with. Each row holds the 32-byte Ed25519 seed,
-- encrypted with AES-256-GCM under a key that never enters this database.
-- The associated data binds a row to its party and fingerprint, so a row
-- copied under another party does not decrypt. The vault keeps its copy.
CREATE TABLE party_key_store (
    party_id    TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    nonce       BLOB NOT NULL,
    ciphertext  BLOB NOT NULL,
    stored_at   INTEGER NOT NULL,
    PRIMARY KEY (party_id, fingerprint)
);
