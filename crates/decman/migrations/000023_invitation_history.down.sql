-- Without the status column every row reads as pending, so history goes first.
DELETE FROM pending_invitations WHERE status != 'pending';
ALTER TABLE pending_invitations DROP COLUMN handled_at;
ALTER TABLE pending_invitations DROP COLUMN status;
