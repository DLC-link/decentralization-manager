-- Extend the cached contract list with package-level metadata so the frontend
-- can render package name + version + creation timestamp without re-querying
-- Canton.
--
-- `package_name` and `created_at` come from the verbose ACS response on the
-- Ledger API; `package_version` is joined in via the participant's Admin
-- PackageService at fetch time.

ALTER TABLE dec_party_contract ADD COLUMN package_name TEXT NOT NULL DEFAULT '';
ALTER TABLE dec_party_contract ADD COLUMN package_version TEXT NOT NULL DEFAULT '';
ALTER TABLE dec_party_contract ADD COLUMN created_at TEXT NOT NULL DEFAULT '';
