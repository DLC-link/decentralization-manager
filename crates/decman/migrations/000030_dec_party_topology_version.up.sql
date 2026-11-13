-- Serial metadata orders independently changing DNS and P2P snapshots.
-- Deferral lets a transaction claim a new party's version before inserting
-- the parent cache row. Existing parties acquire versions on their next read.
CREATE TABLE dec_party_topology_version (
    party_id TEXT PRIMARY KEY NOT NULL
        REFERENCES dec_party(party_id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    namespace_serial INTEGER NOT NULL CHECK (namespace_serial > 0),
    p2p_serial INTEGER NOT NULL CHECK (p2p_serial > 0)
);
