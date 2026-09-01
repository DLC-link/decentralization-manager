-- Dars invitations carry the SHA-256 of each DAR so a peer can refuse a file
-- whose content differs from the one its operator accepted.
ALTER TABLE pending_invitations ADD COLUMN dar_hashes TEXT;
