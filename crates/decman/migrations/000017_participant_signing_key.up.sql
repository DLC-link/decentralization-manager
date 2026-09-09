-- Each member contributes one Daml (protocol) signing key to a decentralized
-- party, but `PartyToParticipant.party_signing_keys` records no owner for any
-- of them and the key is not delegated in topology, so nothing on-chain says
-- whose key is whose. Kick needs that mapping to drop the removed member's
-- key. Peers report their own fingerprint over the OwnerKeys exchange, the
-- same way `owner_key` is resolved, and it is cached here.
ALTER TABLE dec_party_participant ADD COLUMN signing_key TEXT;
