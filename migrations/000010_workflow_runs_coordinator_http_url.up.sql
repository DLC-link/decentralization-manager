-- Issue #173 (durable cancel signal): the peer needs the coordinator's
-- HTTP URL so it can probe coordinator status when Noise is unreachable.
-- Nullable for backward compatibility with rows persisted before the
-- accompanying invite-payload change.
ALTER TABLE workflow_runs ADD COLUMN coordinator_http_url TEXT;
