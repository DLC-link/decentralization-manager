-- One row per attempt to make this node leave an external party.
--
-- The row is written when the attempt starts and updated as it moves, so a
-- crash leaves an `in_progress` row that names the step it reached. `step` is
-- what a later attempt reads to resume, and `resumed_from` links the two.
--
-- `requested_by` is the admin user, or the index of the tenant key in the
-- configured list. It never holds the key itself.
CREATE TABLE external_party_unhost_audit (
    id                        INTEGER PRIMARY KEY AUTOINCREMENT,
    party_id                  TEXT NOT NULL,
    participant_id            TEXT NOT NULL,
    requested_via             TEXT NOT NULL,
    requested_by              TEXT NOT NULL,
    serial_before             INTEGER,
    serial_after              INTEGER,
    validator_rights_archived TEXT NOT NULL DEFAULT '[]',
    preapprovals_cancelled    TEXT NOT NULL DEFAULT '[]',
    splice_cleanup            INTEGER NOT NULL,
    step                      TEXT NOT NULL,
    resumed_from              INTEGER,
    started_at                INTEGER NOT NULL,
    finished_at               INTEGER,
    status                    TEXT NOT NULL,
    error                     TEXT
);

CREATE INDEX idx_external_party_unhost_audit_party
    ON external_party_unhost_audit(party_id, id DESC);
