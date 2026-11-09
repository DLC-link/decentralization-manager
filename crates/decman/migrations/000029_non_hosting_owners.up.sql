-- The participant that reported holding an owner key. A peer that owns the
-- party's namespace without hosting the party has no dec_party_participant
-- row, so its claim is kept here.
ALTER TABLE dec_party_owner ADD COLUMN participant_uid TEXT;

-- The namespace owner key a kick invitation removes.
ALTER TABLE pending_invitations ADD COLUMN kicked_owner_key TEXT;
