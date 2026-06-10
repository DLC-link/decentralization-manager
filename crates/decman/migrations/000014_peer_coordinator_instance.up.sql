-- Concurrent multi-instance workflows: scope coordinator-initiated cancel and
-- retry to a single run.
--
-- A peer-side `workflow_runs` row now records WHICH coordinator run it belongs
-- to (the invite's `workflow_instance`). The CancelInvite / RetryWorkflow
-- listener arms use it so a coordinator cancelling or retrying one of several
-- concurrent runs no longer cancels/retries every run its peers hold from that
-- coordinator. NULL on coordinator-side rows and on rows that predate this
-- migration (legacy unscoped messages still match those).
ALTER TABLE workflow_runs ADD COLUMN coordinator_instance TEXT;
