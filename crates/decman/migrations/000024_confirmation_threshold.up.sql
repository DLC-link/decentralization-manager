-- A party's confirmation threshold can now differ from its namespace
-- threshold. NULL on a cached party means the row predates this column.
ALTER TABLE dec_party ADD COLUMN confirmation_threshold INTEGER;
-- The confirmation threshold an invitation asks the peer to sign. NULL means
-- it equals new_threshold.
ALTER TABLE pending_invitations ADD COLUMN confirmation_threshold INTEGER;
