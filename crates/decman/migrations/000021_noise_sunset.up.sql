-- Noise sunset (design section 8): nodes coordinate only through Canton.
--
-- Statement order matters. The coordinator pubkey is rewritten to a
-- participant id while `peers.public_key` still exists, and the peer columns
-- are dropped last.

-- 2. A Noise-era run cannot resume on this build: the peer command loop and
--    the coordinator state machines are gone.
UPDATE workflow_runs
SET status = 'failed',
    error = 'Interrupted by the Noise-sunset upgrade. Dismiss this card and start the operation again.',
    updated_at = strftime('%s','now')
WHERE status = 'inprogress';

-- 3. Historical cards keep a resolvable coordinator: the Noise pubkey becomes
--    the participant id the peers table mapped it to.
UPDATE workflow_runs
SET coordinator_pubkey = (
    SELECT participant_id FROM peers WHERE peers.public_key = workflow_runs.coordinator_pubkey
)
WHERE coordinator_pubkey IS NOT NULL
  AND EXISTS (SELECT 1 FROM peers WHERE peers.public_key = workflow_runs.coordinator_pubkey);

-- 4.
ALTER TABLE workflow_runs RENAME COLUMN coordinator_pubkey TO coordinator_participant;

-- 5. The on-ledger fields of a run (design D11).
ALTER TABLE workflow_runs ADD COLUMN coordinator_party TEXT;
ALTER TABLE workflow_runs ADD COLUMN proposal_cid TEXT;
ALTER TABLE workflow_runs ADD COLUMN topology_hashes_json TEXT;
ALTER TABLE workflow_runs ADD COLUMN member_variant TEXT;

-- 6. Invitation cards are re-projected from the ledger, so the Noise rows go.
DELETE FROM pending_invitations;
ALTER TABLE pending_invitations RENAME COLUMN coordinator_pubkey TO coordinator_participant;
ALTER TABLE pending_invitations ADD COLUMN coordinator_party TEXT;
ALTER TABLE pending_invitations ADD COLUMN proposal_cid TEXT;
ALTER TABLE pending_invitations ADD COLUMN expires_at INTEGER;

-- 7. A peer is `(participant_id, name, party)` (design D2).
ALTER TABLE peers DROP COLUMN address;
ALTER TABLE peers DROP COLUMN port;
ALTER TABLE peers DROP COLUMN public_key;
