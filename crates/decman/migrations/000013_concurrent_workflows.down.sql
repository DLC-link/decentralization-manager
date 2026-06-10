-- Restore the single-InProgress-run-per-(kind, role) unique index.
CREATE UNIQUE INDEX idx_workflow_runs_inprogress_per_kind
    ON workflow_runs(kind, role) WHERE status = 'inprogress';
