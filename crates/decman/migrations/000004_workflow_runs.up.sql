CREATE TABLE workflow_runs (
    instance_name            TEXT PRIMARY KEY NOT NULL,
    kind                     TEXT NOT NULL,
    role                     TEXT NOT NULL,
    status                   TEXT NOT NULL,
    current_step             TEXT NOT NULL,
    step_index               INTEGER NOT NULL,
    step_total               INTEGER NOT NULL,
    config_json              TEXT NOT NULL,
    coordinator_pubkey       TEXT,
    expected_attestors_json  TEXT NOT NULL,
    completed_attestors_json TEXT NOT NULL,
    dec_party_id             TEXT,
    error                    TEXT,
    dismissed                INTEGER NOT NULL DEFAULT 0,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL
);

CREATE INDEX idx_workflow_runs_status ON workflow_runs(status);

-- Enforces the invariant that only one workflow of a given kind+role is in
-- flight on this node at any moment. Prevents start handlers and accept-invite
-- paths from creating concurrent runs of the same shape.
CREATE UNIQUE INDEX idx_workflow_runs_inprogress_per_kind
    ON workflow_runs(kind, role) WHERE status = 'inprogress';

CREATE TABLE workflow_artifacts (
    instance_name TEXT NOT NULL REFERENCES workflow_runs(instance_name) ON DELETE CASCADE,
    artifact_kind TEXT NOT NULL,
    attestor_id   TEXT NOT NULL DEFAULT '',
    payload       BLOB NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (instance_name, artifact_kind, attestor_id)
);

CREATE INDEX idx_workflow_artifacts_instance ON workflow_artifacts(instance_name);

-- Per-node identity material associated with a dec_party.
--
-- Onboarding generates these artefacts during its run; contracts (and any
-- other later workflow that needs to know "what's my participant_id / signing
-- key for this dec_party") reads them by dec_party_id. Lifecycle is the
-- dec_party itself, not the workflow_run that produced them — the row stays
-- alive even after the originating onboarding row is dismissed.
--
-- attestor_id is the canton id of the participant the artefact belongs to:
--   - on a coordinator's DB after onboarding: one row per (dec_party_id,
--     artifact_kind, attestor_id) for every collected attestor (e.g. all
--     `participant_id` files).
--   - on a node's DB for self-only artefacts (its own DAML signing key
--     export): a single row per (dec_party_id, artifact_kind) with
--     attestor_id = the local participant_id.
CREATE TABLE dec_party_identity (
    dec_party_id  TEXT NOT NULL,
    artifact_kind TEXT NOT NULL,
    attestor_id   TEXT NOT NULL,
    payload       BLOB NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (dec_party_id, artifact_kind, attestor_id)
);

CREATE INDEX idx_dec_party_identity_dec_party
    ON dec_party_identity(dec_party_id);
