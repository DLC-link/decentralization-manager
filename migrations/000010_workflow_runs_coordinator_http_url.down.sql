-- SQLite has had ALTER TABLE DROP COLUMN since 3.35.0 (2021). Project
-- targets a modern SQLite; if the runtime SQLite is older, the migration
-- runner will surface the error.
ALTER TABLE workflow_runs DROP COLUMN coordinator_http_url;
