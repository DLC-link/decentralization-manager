-- The notification feed reads the newest audit rows across every party. The
-- only other index leads with party_id, so without this one that read sorts the
-- whole table. SQLite appends the rowid to every index entry, so scanning this
-- index backwards yields `created_at DESC, id DESC` with no sort step.
CREATE INDEX IF NOT EXISTS idx_governance_audit_created
    ON governance_audit(created_at);
