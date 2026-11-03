-- One row per tenant API call that writes topology or state on this node, and
-- one per ACS import that ended before its final block.
--
-- A call refused after its key check is recorded too, with its status and
-- error. `key_index` is the index of the tenant key in the configured list,
-- and `key_id` is the first 16 hex characters of the key's SHA-256, which
-- still names the key after the list is reordered. Both are NULL in insecure
-- mode, where no key is checked. No column holds the key, a signature or
-- topology bytes. `http_status` is NULL for an import that the node ended on
-- its own, such as one the reaper removed.
CREATE TABLE tenant_call_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    called_at   INTEGER NOT NULL,
    key_index   INTEGER,
    key_id      TEXT,
    endpoint    TEXT NOT NULL,
    party_id    TEXT,
    http_status INTEGER,
    error       TEXT
);

CREATE INDEX idx_tenant_call_audit_party ON tenant_call_audit(party_id, id DESC);
