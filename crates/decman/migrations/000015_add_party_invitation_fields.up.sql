-- AddParty invitations carry the participant being added so the peer card
-- can render "adding X to dec party Y" (mirrors 000008's kick fields).
ALTER TABLE pending_invitations ADD COLUMN new_participant TEXT;
