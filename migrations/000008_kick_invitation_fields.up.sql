ALTER TABLE pending_invitations ADD COLUMN kicked_participant TEXT;
ALTER TABLE pending_invitations ADD COLUMN new_threshold INTEGER;
ALTER TABLE pending_invitations ADD COLUMN previous_threshold INTEGER;
