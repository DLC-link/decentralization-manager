-- Manual rollback (design D12). Lossy: the Noise peer columns come back
-- empty, and `coordinator_participant` values stay participant ids.
ALTER TABLE peers ADD COLUMN address TEXT NOT NULL DEFAULT '';
ALTER TABLE peers ADD COLUMN port INTEGER NOT NULL DEFAULT 9000;
ALTER TABLE peers ADD COLUMN public_key TEXT NOT NULL DEFAULT '';

ALTER TABLE pending_invitations DROP COLUMN expires_at;
ALTER TABLE pending_invitations DROP COLUMN proposal_cid;
ALTER TABLE pending_invitations DROP COLUMN coordinator_party;
ALTER TABLE pending_invitations RENAME COLUMN coordinator_participant TO coordinator_pubkey;

ALTER TABLE workflow_runs DROP COLUMN member_variant;
ALTER TABLE workflow_runs DROP COLUMN topology_hashes_json;
ALTER TABLE workflow_runs DROP COLUMN proposal_cid;
ALTER TABLE workflow_runs DROP COLUMN coordinator_party;
ALTER TABLE workflow_runs RENAME COLUMN coordinator_participant TO coordinator_pubkey;
