-- A decentralized party this participant used to host and no longer does.
--
-- Separate from dec_party on purpose. dec_party is the cache of parties this
-- node hosts now, and the kick, owner-key and membership code all read it with
-- that meaning. A refresh deletes a party from it the moment the node stops
-- hosting it, which is exactly when the node's leftover contracts for it start
-- to go stale.
--
-- The purge columns record the one-off cleanup of those leftovers. A row is
-- deleted when the node hosts the party again, and hidden once an operator
-- dismisses it after a successful purge.
CREATE TABLE IF NOT EXISTS former_dec_party (
    party_id          TEXT    PRIMARY KEY,
    -- Unix seconds: when the first mapping without this participant took effect.
    removed_at        INTEGER NOT NULL,
    detected_at       INTEGER NOT NULL,
    -- 'none' | 'running' | 'succeeded' | 'failed'
    purge_status      TEXT    NOT NULL DEFAULT 'none',
    purge_started_at  INTEGER,
    purge_finished_at INTEGER,
    purged_count      INTEGER,
    kept_count        INTEGER,
    purge_error       TEXT,
    dismissed_at      INTEGER
);
