-- Issue #460: the audit trail rendered every Daml map as
-- {"_unsupported":"map"} and dropped the entries. Cached rows still hold that
-- marker. The cache is written with INSERT OR IGNORE and is read in preference
-- to the ledger, so a fixed build never replaces them. Delete the affected
-- rows. The next chain audit read re-derives them from the ledger.
--
-- LIKE treats `_` as a single-character wildcard, so the marker needs ESCAPE.
DELETE FROM chain_audit_cache
WHERE details LIKE '%"\_unsupported":"map"%' ESCAPE '\';
