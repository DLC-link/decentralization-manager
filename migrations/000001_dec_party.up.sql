CREATE TABLE IF NOT EXISTS dec_party (
    party_id   TEXT PRIMARY KEY NOT NULL,
    prefix     TEXT NOT NULL,
    threshold  INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS dec_party_owner (
    dec_party_id TEXT NOT NULL REFERENCES dec_party(party_id) ON DELETE CASCADE,
    owner_key    TEXT NOT NULL,
    PRIMARY KEY (dec_party_id, owner_key)
);

CREATE TABLE IF NOT EXISTS dec_party_participant (
    dec_party_id    TEXT NOT NULL REFERENCES dec_party(party_id) ON DELETE CASCADE,
    participant_uid TEXT NOT NULL,
    permission      TEXT NOT NULL,
    owner_key       TEXT,
    PRIMARY KEY (dec_party_id, participant_uid)
);

CREATE TABLE IF NOT EXISTS dec_party_contract (
    dec_party_id TEXT NOT NULL REFERENCES dec_party(party_id) ON DELETE CASCADE,
    contract_id  TEXT NOT NULL,
    template_id  TEXT NOT NULL,
    package_id   TEXT NOT NULL,
    PRIMARY KEY (dec_party_id, contract_id)
);

CREATE INDEX IF NOT EXISTS idx_dec_party_prefix ON dec_party(prefix);
CREATE INDEX IF NOT EXISTS idx_party_credentials_dec_party ON party_credentials(dec_party_id);
