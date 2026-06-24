CREATE TABLE IF NOT EXISTS peers (
    participant_id TEXT PRIMARY KEY NOT NULL,
    name           TEXT NOT NULL,
    address        TEXT NOT NULL,
    port           INTEGER NOT NULL,
    public_key     TEXT NOT NULL,
    party          TEXT
);

CREATE TABLE IF NOT EXISTS party_credentials (
    dec_party_id             TEXT PRIMARY KEY NOT NULL,
    member_party_id          TEXT NOT NULL,
    user_id                  TEXT NOT NULL,
    keycloak_url             TEXT NOT NULL DEFAULT '',
    keycloak_realm           TEXT NOT NULL DEFAULT '',
    keycloak_client_id       TEXT NOT NULL DEFAULT '',
    keycloak_client_secret   TEXT,
    keycloak_username        TEXT,
    keycloak_password        TEXT
);
