-- Remember failed archival attempts independently of request and audit lifetimes.
CREATE TABLE governance_mediator_timeout (
    party_id TEXT NOT NULL,
    contract_id TEXT NOT NULL,
    unresponsive TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    PRIMARY KEY (party_id, contract_id)
);
