CREATE TABLE IF NOT EXISTS pending_invitations (
    id                 TEXT PRIMARY KEY NOT NULL,
    invitation_type    TEXT NOT NULL,
    coordinator_pubkey TEXT NOT NULL,
    received_at        INTEGER NOT NULL,
    prefix             TEXT,
    participants       TEXT,
    dar_filenames      TEXT
);

CREATE INDEX IF NOT EXISTS idx_pending_invitations_coordinator
    ON pending_invitations(coordinator_pubkey);
