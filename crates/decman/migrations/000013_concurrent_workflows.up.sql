-- Concurrent multi-instance workflows.
--
-- Migration 000004 added a partial unique index that allowed at most one
-- InProgress run per (kind, role). The server now runs any number of workflows
-- concurrently, each addressed by its own `instance_name` and routed
-- independently over the always-on Noise listener, so this constraint no longer
-- holds. Drop it; runtime concurrency is governed by the in-memory
-- `WorkflowRegistry`, not the database.
DROP INDEX IF EXISTS idx_workflow_runs_inprogress_per_kind;
