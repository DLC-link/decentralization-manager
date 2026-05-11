-- Rename "attestor" → "peer" across the schema. The two terms have always
-- referred to the same set of nodes (other participants in a workflow); this
-- migration unifies the vocabulary on "peer" and rewrites existing rows so
-- the API and frontend can stop carrying the old name.

-- workflow_runs: rename column names and rewrite the persisted role enum.
ALTER TABLE workflow_runs RENAME COLUMN expected_attestors_json TO expected_peers_json;
ALTER TABLE workflow_runs RENAME COLUMN completed_attestors_json TO completed_peers_json;
UPDATE workflow_runs SET role = 'Peer' WHERE role = 'Attestor';

-- workflow_artifacts: rename the attestor_id column. SQLite supports
-- RENAME COLUMN even when the column is part of a PRIMARY KEY since 3.25.
ALTER TABLE workflow_artifacts RENAME COLUMN attestor_id TO peer_id;

-- dec_party_identity: same treatment as workflow_artifacts.
ALTER TABLE dec_party_identity RENAME COLUMN attestor_id TO peer_id;

-- Rewrite the artifact_kind constant carried in both artefact tables.
UPDATE workflow_artifacts SET artifact_kind = 'peer_public_keys' WHERE artifact_kind = 'attestor_public_keys';
UPDATE dec_party_identity SET artifact_kind = 'peer_public_keys' WHERE artifact_kind = 'attestor_public_keys';
