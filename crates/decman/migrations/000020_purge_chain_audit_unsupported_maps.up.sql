-- Issue #460: the audit trail rendered every Daml map as
-- {"_unsupported":"map"} and dropped the entries. Cached rows still hold that
-- marker. The cache is written with INSERT OR IGNORE and is read in preference
-- to the ledger, so a fixed build never replaces them. Delete the affected
-- rows. The next chain audit read re-derives them from the ledger.
--
-- The purge takes every row of an affected party, not only the marked rows.
-- A cached page answers from whatever rows it finds, and it falls through to
-- the ledger only when it finds none. Deleting a single row would leave the
-- rest of its offset group in place, so that page would keep answering from
-- the cache and the deleted entry would never come back.
--
-- LIKE treats `_` as a single-character wildcard, so the marker needs ESCAPE.
DELETE FROM chain_audit_cache
WHERE party_id IN (
    SELECT party_id FROM chain_audit_cache
    WHERE details LIKE '%"\_unsupported":"map"%' ESCAPE '\'
);
