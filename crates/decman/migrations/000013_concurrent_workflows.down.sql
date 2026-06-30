-- Restore the single-InProgress-run-per-(kind, role) unique index.
--
-- NOTE: this downgrade FAILS if the database currently holds more than one
-- 'inprogress' run for the same (kind, role) — exactly what the up migration
-- allows. Before downgrading, finalize or dismiss the extra concurrent runs
-- (set their status terminal) so at most one 'inprogress' row per (kind, role)
-- remains, otherwise the unique index creation errors.
CREATE UNIQUE INDEX idx_workflow_runs_inprogress_per_kind
    ON workflow_runs(kind, role) WHERE status = 'inprogress';
