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

CREATE INDEX IF NOT EXISTS idx_governance_audit_party_id ON governance_audit(party_id);
CREATE INDEX IF NOT EXISTS idx_governance_audit_created_at ON governance_audit(created_at);
