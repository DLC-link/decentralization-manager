-- One row per tenant API call that writes topology or state on this node.
--
-- A call refused after its key check is recorded too, with its status and
-- error. `key_index` is the index of the tenant key in the configured list,
-- or NULL in insecure mode, where no key is checked. No column holds the key,
-- a signature or topology bytes.
CREATE TABLE tenant_call_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    called_at   INTEGER NOT NULL,
    key_index   INTEGER,
    endpoint    TEXT NOT NULL,
    party_id    TEXT,
    http_status INTEGER NOT NULL,
    error       TEXT
);

CREATE INDEX idx_tenant_call_audit_party ON tenant_call_audit(party_id, id DESC);
