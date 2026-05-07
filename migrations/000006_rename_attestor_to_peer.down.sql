-- Reverse 000006: rename peer → attestor on the schema and rewrite rows.
UPDATE dec_party_identity SET artifact_kind = 'attestor_public_keys' WHERE artifact_kind = 'peer_public_keys';
UPDATE workflow_artifacts SET artifact_kind = 'attestor_public_keys' WHERE artifact_kind = 'peer_public_keys';

ALTER TABLE dec_party_identity RENAME COLUMN peer_id TO attestor_id;
ALTER TABLE workflow_artifacts RENAME COLUMN peer_id TO attestor_id;

UPDATE workflow_runs SET role = 'Attestor' WHERE role = 'Peer';
ALTER TABLE workflow_runs RENAME COLUMN completed_peers_json TO completed_attestors_json;
ALTER TABLE workflow_runs RENAME COLUMN expected_peers_json TO expected_attestors_json;
