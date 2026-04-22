ALTER TABLE dec_party ADD COLUMN my_owner_key TEXT;

CREATE TABLE IF NOT EXISTS governance_audit (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp       INTEGER NOT NULL,
    event_type      TEXT NOT NULL,
    party_id        TEXT NOT NULL,
    member_party_id TEXT NOT NULL,
    governance_type TEXT NOT NULL,
    action_summary  TEXT NOT NULL,
    details         TEXT NOT NULL,
    status          TEXT NOT NULL,
    error_message   TEXT,
    created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_governance_audit_party_created ON governance_audit(party_id, created_at DESC);

CREATE TABLE IF NOT EXISTS chain_audit_cache (
    party_id        TEXT NOT NULL,
    offset          INTEGER NOT NULL,
    timestamp       INTEGER NOT NULL,
    event_type      TEXT NOT NULL,
    contract_id     TEXT NOT NULL,
    template_id     TEXT NOT NULL,
    package_id      TEXT NOT NULL,
    governance_type TEXT NOT NULL,
    action_summary  TEXT NOT NULL,
    choice          TEXT,
    acting_parties  TEXT NOT NULL,
    update_id       TEXT NOT NULL,
    details         TEXT NOT NULL,
    PRIMARY KEY (party_id, offset, contract_id, event_type)
);

CREATE INDEX IF NOT EXISTS idx_chain_audit_cache_party_id ON chain_audit_cache(party_id);
