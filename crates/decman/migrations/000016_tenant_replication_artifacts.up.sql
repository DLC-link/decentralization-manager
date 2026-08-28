-- Durable artefacts for a replication that has no workflow run behind it.
--
-- `workflow_artifacts.instance_name` is a foreign key into `workflow_runs`,
-- which is correct for the Noise workflows: every artefact belongs to a run the
-- coordinator persisted. The tenant API has no run — it is stateless HTTP with
-- Canton as the coordination store — so its replication offsets and its
-- disconnect-window marker have nothing to point at and the foreign key refuses
-- them.
--
-- Same shape, no foreign key. Keyed by a name derived from the (party, target)
-- pair rather than a run id, so every host computes the same key without
-- coordinating.
CREATE TABLE tenant_replication_artifacts (
    instance_name TEXT NOT NULL,
    artifact_kind TEXT NOT NULL,
    attestor_id   TEXT NOT NULL DEFAULT '',
    payload       BLOB NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (instance_name, artifact_kind, attestor_id)
);

CREATE INDEX idx_tenant_replication_artifacts_instance
    ON tenant_replication_artifacts(instance_name);
