-- A participant that was fed part of an ACS before the transfer failed.
--
-- Deliberately NOT in workflow_artifacts. That table is keyed by
-- instance_name and carries `REFERENCES workflow_runs(...) ON DELETE CASCADE`,
-- so a marker written there records a fact about one *run*: a fresh add-party
-- gets a new instance name and cannot see it, and dismissing the failed run
-- deletes it outright. What has to be remembered here is a fact about the
-- *participant*, which outlives both.
--
-- Cleared only by an operator, once the participant has been repaired or
-- restored, because Canton offers no way to ask how much of a cancelled import
-- it committed.
CREATE TABLE IF NOT EXISTS acs_import_quarantine (
    party_id       TEXT    NOT NULL,
    participant_id TEXT    NOT NULL,
    reason         TEXT    NOT NULL,
    bytes_imported INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    PRIMARY KEY (party_id, participant_id)
);
