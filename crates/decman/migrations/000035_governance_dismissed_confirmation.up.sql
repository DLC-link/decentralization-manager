-- Confirmations an operator hid on this node because they cannot be archived.
-- Keyed by confirmation, so a new confirmation on the same action shows it again.
CREATE TABLE governance_dismissed_confirmation (
    party_id TEXT NOT NULL,
    contract_id TEXT NOT NULL,
    dismissed_by TEXT NOT NULL,
    dismissed_at INTEGER NOT NULL,
    PRIMARY KEY (party_id, contract_id)
);
