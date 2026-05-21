-- Reinstate the single-tenant invariant: at most one InProgress run per
-- (kind, role). Mirrors the original index from 000004.
CREATE UNIQUE INDEX idx_workflow_runs_inprogress_per_kind
    ON workflow_runs(kind, role) WHERE status = 'inprogress';
