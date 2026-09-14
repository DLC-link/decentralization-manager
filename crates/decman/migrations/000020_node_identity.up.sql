-- Node identity (design D1) and the proposal-decision guard (design D11).
--
-- A node identity is one `party_credentials` row with `kind = 'node'`. Its
-- `dec_party_id` and `member_party_id` both hold the node party, so
-- `AuthRegistry::get(node_party)` returns its token manager unchanged. Every
-- existing row is a decparty row, so the column defaults to 'decparty'.
ALTER TABLE party_credentials ADD COLUMN kind TEXT NOT NULL DEFAULT 'decparty';

-- One row per `WorkflowProposal` this node has decided on. The row stops the
-- observer from re-projecting a dismissed or declined proposal into
-- `pending_invitations`, and it pins the topology transaction hashes the node
-- agreed to co-sign, so a later tick never signs from a cached match.
CREATE TABLE proposal_decisions (
    proposal_cid TEXT PRIMARY KEY NOT NULL,
    decision TEXT NOT NULL,
    decided_at INTEGER NOT NULL,
    pinned_hashes_json TEXT
);
