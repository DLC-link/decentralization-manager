-- The coordinator's Daml key that a rotate-key invitation replaces.
ALTER TABLE pending_invitations ADD COLUMN rotated_key TEXT;
