-- This node's node party and the Ledger API login that acts as it.
--
-- Separate from party_credentials on purpose. Every reader of that table
-- treats a row as a decentralized party: inbound token validation trusts its
-- issuer, and an empty table switches inbound auth off. The node login must
-- never become a trusted inbound issuer.
--
-- One row at most. Either the Keycloak or the Auth0 columns are set, never
-- both. Neither is set in insecure mode, where the node presents the unsafe
-- token. Client ids and secrets are encrypted like those in party_credentials.
CREATE TABLE node_identity (
    id                     INTEGER PRIMARY KEY CHECK (id = 1),
    node_party_id          TEXT    NOT NULL,
    participant_id         TEXT    NOT NULL,
    user_id                TEXT    NOT NULL,
    keycloak_url           TEXT,
    keycloak_realm         TEXT,
    keycloak_client_id     TEXT,
    keycloak_client_secret TEXT,
    auth0_domain           TEXT,
    auth0_audience         TEXT,
    auth0_client_id        TEXT,
    auth0_client_secret    TEXT,
    -- Unix seconds.
    created_at             INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL
);
