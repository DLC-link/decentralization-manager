-- Accepted and declined invitations stay in this table as history, so the
-- notification feed can list them under Completed. Every read and delete of
-- the pending set filters on status = 'pending', so it is unchanged. Rows
-- written before this migration are all pending, which the default records.
ALTER TABLE pending_invitations ADD COLUMN status TEXT NOT NULL DEFAULT 'pending';
ALTER TABLE pending_invitations ADD COLUMN handled_at INTEGER;
