use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::{decode::decode_party_to_participant, expectations::PeerExpectations};
use crate::{canton_id::CantonId, error::Result};

impl PeerExpectations {
    /// Validate the add-party onboarding-flag clearing proposal: same party,
    /// same members, same threshold, and every hosting participant actually
    /// clear of the onboarding flag.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub fn check_clear_onboarding(&self, payload: &[u8]) -> Result {
        let dec_party_id = self.dec_party_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!("accepted add-party invitation carries no dec party id")
        })?;
        let mapping = decode_party_to_participant(payload)?;

        if mapping.party != dec_party_id.to_string() {
            anyhow::bail!(
                "clearing proposal is for party {found} but the accepted invitation is for \
                 {dec_party_id}",
                found = mapping.party
            );
        }
        self.check_p2p_membership(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;

        if let Some(still_onboarding) = mapping
            .participants
            .iter()
            .find(|participant| participant.onboarding.is_some())
        {
            anyhow::bail!(
                "clearing proposal still flags {participant} as onboarding",
                participant = still_onboarding.participant_uid
            );
        }
        Ok(())
    }

    /// Validate the DAR files a coordinator sends before they are uploaded and
    /// vetted on this node's participant.
    ///
    /// # Errors
    ///
    /// Errors if a file was not named in the accepted invitation, if an
    /// accepted file is missing, or if a file's content does not hash to the
    /// value the invitation pinned.
    pub fn check_dars(&self, files: &[(String, Vec<u8>)]) -> Result {
        // An invitation that named no DARs authorizes no DARs. Only the hash
        // list is treated leniently for an older coordinator — `dar_filenames`
        // has been on the invite since the workflow existed, so an empty one
        // paired with actual DAR bytes is a coordinator installing code the
        // operator never saw, not a version skew.
        if self.dar_filenames.is_empty() {
            if files.is_empty() {
                return Ok(());
            }
            anyhow::bail!(
                "coordinator sent {count} DAR(s) but the accepted invitation named none",
                count = files.len()
            );
        }

        // Both sides are compared as sets, so a repeated name would collapse and
        // mask a missing or extra file. Refuse the ambiguity rather than pick a
        // reading of it.
        let accepted: BTreeSet<&str> = self.dar_filenames.iter().map(String::as_str).collect();
        if accepted.len() != self.dar_filenames.len() {
            anyhow::bail!("accepted invitation names the same DAR twice");
        }
        let received: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        if received.len() != files.len() {
            anyhow::bail!("coordinator sent the same DAR filename twice");
        }
        if let Some(unexpected) = received.difference(&accepted).next() {
            anyhow::bail!("coordinator sent DAR {unexpected}, which the invitation did not name");
        }
        if let Some(missing) = accepted.difference(&received).next() {
            anyhow::bail!("coordinator did not send DAR {missing} named by the invitation");
        }

        // Only a wholly absent hash list means "coordinator predates the field".
        // A partial one cannot be matched up with the filenames, so treating it
        // as legacy would silently drop the content pin.
        if self.dar_hashes.is_empty() {
            tracing::warn!(
                "accepted Dars invitation carried no DAR hashes; the filenames match but \
                 the content cannot be pinned (the coordinator predates the field)"
            );
            return Ok(());
        }
        if self.dar_hashes.len() != self.dar_filenames.len() {
            anyhow::bail!(
                "accepted invitation carries {hashes} DAR hash(es) for {names} filename(s), \
                 so the content cannot be pinned",
                hashes = self.dar_hashes.len(),
                names = self.dar_filenames.len()
            );
        }

        for (filename, data) in files {
            let expected = self
                .dar_filenames
                .iter()
                .zip(&self.dar_hashes)
                .find(|(name, _)| *name == filename)
                .map(|(_, hash)| hash)
                .ok_or_else(|| anyhow::anyhow!("no accepted hash for DAR {filename}"))?;
            let actual = hex::encode(Sha256::digest(data));
            if &actual != expected {
                anyhow::bail!(
                    "DAR {filename} hashes to {actual} but the invitation pinned {expected}"
                );
            }
        }

        tracing::info!(
            "all {count} DAR(s) match the accepted invitation",
            count = files.len()
        );
        Ok(())
    }

    /// The dec party a coordinator names in a workflow config must be the one
    /// the operator accepted. Without this a peer would happily sign with its
    /// keys for a party it never agreed to act for.
    ///
    /// # Errors
    ///
    /// Errors if the party differs from the accepted one.
    pub fn check_dec_party(&self, party: &CantonId) -> Result {
        match &self.dec_party_id {
            Some(expected) if expected != party => anyhow::bail!(
                "coordinator sent a config for party {party} but the accepted invitation \
                 is for {expected}"
            ),
            Some(_) => Ok(()),
            // Every invitation kind that reaches this check carries the dec
            // party as a required field, so an absent one is not version skew —
            // it means the invite never parsed and the peer cannot say what its
            // operator agreed to. `check_party_proposals` already refuses that;
            // this refuses it too rather than accepting whatever party the
            // coordinator names.
            None => anyhow::bail!(
                "accepted {kind:?} invitation carries no dec party id, so the coordinator's \
                 config cannot be checked against it",
                kind = self.kind
            ),
        }
    }

    /// The participant an add-party run introduces must be the one named in
    /// the accepted invitation.
    ///
    /// # Errors
    ///
    /// Errors if the participant differs from the accepted one.
    pub fn check_new_participant(&self, participant: &CantonId) -> Result {
        match &self.new_participant {
            Some(expected) if expected != participant => anyhow::bail!(
                "coordinator is adding {participant} but the accepted invitation names \
                 {expected}"
            ),
            Some(_) => Ok(()),
            // `new_participant` is required on every add-party invite, so an
            // absent one means the invite never parsed — not that it predates
            // the field. Without it the coordinator would pick who joins.
            None => anyhow::bail!(
                "accepted add-party invitation names no new participant, so the coordinator's \
                 choice cannot be checked against it"
            ),
        }
    }
}

/// SHA-256 of a DAR's bytes, hex encoded — the value a coordinator pins in the
/// invitation and a peer recomputes before it uploads and vets the file.
pub fn hash_dar(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
        party_to_participant::hosting_participant, topology_mapping,
    };

    use super::*;
    use crate::workflow::validation::test_support::*;

    /// The party is required on every invitation kind that reaches this check,
    /// so an absent one means the invite never parsed — the peer cannot say
    /// what it agreed to and must not accept the coordinator's word for it.
    #[test]
    fn rejects_a_config_when_no_party_was_accepted() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.dec_party_id.is_none());
        assert!(expectations.check_dec_party(&canton_id("any", 4)?).is_err());
        Ok(())
    }

    #[test]
    fn rejects_an_add_party_config_when_no_participant_was_accepted() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.new_participant.is_none());
        assert!(
            expectations
                .check_new_participant(&canton_id("any", 4)?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_clearing_proposal_that_still_flags_onboarding() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let party = canton_id("dec", 7)?;
        let mut expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.dec_party_id = Some(party.clone());

        let mut mapping = p2p(&party.to_string(), &[a, b], 2);
        // Keep the mapping otherwise valid so the assertion below is about the
        // onboarding flag, not about an earlier check tripping first.
        mapping.party_signing_keys = Some(signing_keys(2, 2));
        mapping.participants[1].onboarding = Some(hosting_participant::Onboarding::default());
        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(mapping));

        let error = expectations
            .check_clear_onboarding(&payload)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("still flags"), "unexpected error: {error}");
        Ok(())
    }

    /// A repeated filename collapses in the set comparison and would mask a
    /// missing or extra file, so the ambiguity is refused outright.
    #[test]
    fn rejects_duplicate_dar_filenames() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string(), "gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes"), hash_dar(b"gov-bytes")];
        assert!(
            expectations
                .check_dars(&[("gov.dar".to_string(), b"gov-bytes".to_vec())])
                .is_err()
        );

        expectations.dar_filenames = vec!["a.dar".to_string(), "b.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"a"), hash_dar(b"b")];
        assert!(
            expectations
                .check_dars(&[
                    ("a.dar".to_string(), b"a".to_vec()),
                    ("a.dar".to_string(), b"a".to_vec()),
                ])
                .is_err()
        );
        Ok(())
    }

    /// Only a wholly absent hash list means "older coordinator". A partial one
    /// cannot be lined up with the filenames, so it must not read as legacy.
    #[test]
    fn rejects_a_partial_dar_hash_list() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["a.dar".to_string(), "b.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"a")];
        assert!(
            expectations
                .check_dars(&[
                    ("a.dar".to_string(), b"a".to_vec()),
                    ("b.dar".to_string(), b"b".to_vec()),
                ])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn accepts_dars_matching_the_pinned_hashes() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes")];
        expectations.check_dars(&[("gov.dar".to_string(), b"gov-bytes".to_vec())])
    }

    /// Same filename, different bytes: the DAR the operator saw named is not
    /// the DAR that arrived.
    #[test]
    fn rejects_a_swapped_dar() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes")];
        assert!(
            expectations
                .check_dars(&[("gov.dar".to_string(), b"malicious-bytes".to_vec())])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_an_extra_dar() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes")];
        assert!(
            expectations
                .check_dars(&[
                    ("gov.dar".to_string(), b"gov-bytes".to_vec()),
                    ("extra.dar".to_string(), b"whatever".to_vec()),
                ])
                .is_err()
        );
        Ok(())
    }

    /// The hole this closes: an invitation naming no DARs, accepted as
    /// harmless, followed by DAR bytes the peer would have uploaded and vetted
    /// without any check at all.
    #[test]
    fn rejects_dars_no_invitation_named() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.dar_filenames.is_empty());
        assert!(
            expectations
                .check_dars(&[("surprise.dar".to_string(), b"arbitrary-code".to_vec())])
                .is_err()
        );
        // Nothing accepted and nothing sent stays fine — that is a Dars run
        // with no files, not a bypass.
        expectations.check_dars(&[])
    }

    /// An older coordinator sends filenames but no hashes: the filename check
    /// still applies, the content check is skipped with a warning.
    #[test]
    fn tolerates_an_invitation_without_dar_hashes() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.check_dars(&[("gov.dar".to_string(), b"anything".to_vec())])?;
        assert!(
            expectations
                .check_dars(&[("other.dar".to_string(), b"anything".to_vec())])
                .is_err()
        );
        Ok(())
    }
}
