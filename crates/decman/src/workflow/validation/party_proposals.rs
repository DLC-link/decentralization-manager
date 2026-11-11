use std::collections::{BTreeMap, BTreeSet};

use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    DecentralizedNamespaceDefinition, PartyToParticipant,
};
use sqlx::SqlitePool;

use super::{
    decode::{decode_namespace_definition, decode_party_to_participant, decode_proposal_serial},
    expectations::PeerExpectations,
    head::{Departing, OwnerRecords, SynchronizerHead, check_against_head},
    own_keys::WhenUnrecorded,
};
use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    server::WorkflowKind,
    utils,
    workflow::{
        onboarding::steps::proposals::create::decode_keys_payload,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

impl PeerExpectations {
    /// Validate the DNS + P2P proposal pair of a kick, add-party or
    /// change-threshold run.
    ///
    /// These run against a party that already exists, so the invitation pins
    /// the party id itself: both mappings must target exactly that party, with
    /// exactly the accepted member set and the accepted threshold.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub async fn check_party_proposals(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        dns_payload: &[u8],
        p2p_payload: &[u8],
    ) -> Result {
        let dec_party_id = self.dec_party_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted {kind:?} invitation carries no dec party id, so the proposals \
                 cannot be checked against it",
                kind = self.kind
            )
        })?;

        let namespace_def = decode_namespace_definition(dns_payload)?;
        self.check_party_dns_shape(dec_party_id, &namespace_def)?;

        // Losing our namespace from the owner set would leave this node hosting
        // a party it can no longer authorize changes to. The lookup itself is
        // best-effort — a party onboarded before the identity table existed may
        // have no local record — but a namespace we *can* resolve must be there.
        // A lookup or decode failure is an error, not a legacy party: letting
        // it through would hand a coordinator this peer's signature on a
        // proposal that drops its own namespace.
        let own_namespace = self.own_namespace(config, storage, instance_name).await?;
        match &own_namespace {
            Some(own_namespace)
                if !namespace_def
                    .owners
                    .iter()
                    .any(|owner| *own_namespace == *owner) =>
            {
                anyhow::bail!(
                    "DNS proposal drops this node's namespace {own_namespace} from the owner set"
                )
            }
            Some(_) => {}
            None => tracing::warn!(
                "this node's keys are unrecorded for this party (onboarded before the \
                 identity table existed); skipping the owner-set membership check"
            ),
        }

        let mapping = decode_party_to_participant(p2p_payload)?;
        if mapping.party != dec_party_id.to_string() {
            anyhow::bail!(
                "P2P proposal is for party {found} but the accepted invitation is for \
                 {dec_party_id}",
                found = mapping.party
            );
        }
        self.check_p2p_membership(&mapping)?;
        self.check_onboarding_markers(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;
        // Kick rebuilds the key set to take the departing member's key out
        // (#428), and it has to work out which key that is from local records.
        // Get it wrong and a remaining member's key goes instead: the counts
        // still match, so only the member whose key vanished can catch it.
        self.check_own_daml_key(
            config,
            storage,
            instance_name,
            &mapping,
            WhenUnrecorded::Skip,
        )
        .await?;
        let new_member = self.new_member_fingerprints(storage, instance_name).await?;
        let records = if self.kind == WorkflowKind::Kick {
            Some(
                self.owner_records(storage, dec_party_id, own_namespace.as_ref())
                    .await?,
            )
        } else {
            None
        };
        self.check_on_chain_delta(
            config,
            dec_party_id,
            (decode_proposal_serial(dns_payload)?, &namespace_def),
            (decode_proposal_serial(p2p_payload)?, &mapping),
            new_member
                .as_ref()
                .map(|(namespace, daml)| (namespace.as_str(), daml.as_str())),
            records.as_ref(),
        )
        .await?;

        tracing::info!(
            "{kind:?} proposals match the accepted invitation for {dec_party_id}",
            kind = self.kind
        );
        Ok(())
    }

    /// The checks on a party DNS proposal that need no topology read: the
    /// accepted namespace, distinct owners, and the accepted threshold.
    ///
    /// The owner count is not held to the member count. A member can own the
    /// namespace without hosting the party, so the two differ on a healthy
    /// party. [`check_on_chain_delta`](Self::check_on_chain_delta) pins the
    /// owner set instead, through the owner key a kick invitation names.
    ///
    /// A kick invitation without that key comes from a coordinator that
    /// predates it, or from one that left it out. Nothing then ties the owners
    /// the kick keeps to the members it keeps, so the proposal must keep one
    /// owner per member, as before owner keys were named.
    fn check_party_dns_shape(
        &self,
        dec_party_id: &CantonId,
        namespace_def: &DecentralizedNamespaceDefinition,
    ) -> Result {
        if namespace_def.decentralized_namespace != dec_party_id.namespace.to_hex() {
            anyhow::bail!(
                "DNS proposal targets namespace {found} but the accepted invitation is for \
                 {expected}",
                found = namespace_def.decentralized_namespace,
                expected = dec_party_id.namespace.to_hex()
            );
        }
        // Canton treats the owners as a set, so a repeated namespace would keep
        // the vector length the invitation expects while shrinking the real
        // owner set — and with it the number of distinct signatures the
        // threshold represents.
        let owners: BTreeSet<&String> = namespace_def.owners.iter().collect();
        if owners.len() != namespace_def.owners.len() {
            anyhow::bail!("DNS proposal repeats an owner namespace");
        }
        if self.kind == WorkflowKind::Kick
            && self.kicked_owner_key.is_none()
            && owners.len() != self.members.len()
        {
            anyhow::bail!(
                "DNS proposal has {found} owners but the accepted invitation names {expected} \
                 members and no owner key to remove",
                found = owners.len(),
                expected = self.members.len()
            );
        }
        self.check_threshold("namespace", namespace_def.threshold, owners.len())
    }

    /// Hold both proposals to the party's on-chain state (#420, #422).
    ///
    /// Each signer only sees its own namespace and its own Daml key, so an
    /// owner or key belonging to a member that does not sign this round could
    /// otherwise be swapped for one the coordinator controls. Reading the
    /// current definition and mapping pins everything else to what is already
    /// on-chain plus the one change the run is for. A read that fails refuses
    /// the proposal: a peer that cannot check it must not sign it.
    ///
    /// Only the new member holds the new member's keys, so only it can pin the
    /// added owner and key to them; every other peer checks that at most one
    /// was added. Its refusal blocks the P2P, which Canton makes an added host
    /// sign, but not the DNS: an added owner is authorized by its own key and
    /// the existing owners' quorum, not by the new participant.
    ///
    /// A proposal at the head serial that equals the head mapping is already
    /// effective, so signing it changes nothing and it skips the delta. Only a
    /// coordinator built before #448 sends one: it proposed with `Authorize`
    /// against the synchronizer, which puts a threshold-1 namespace in force
    /// at once. Any other proposal must sit at the next serial.
    ///
    /// `new_member` is this node's `(namespace, daml)` fingerprints when it is
    /// the new member. `records` are the owner keys this node has on record,
    /// which a kick checks the owner key the invitation names against.
    async fn check_on_chain_delta(
        &self,
        config: &NodeConfig,
        dec_party_id: &CantonId,
        dns: (u32, &DecentralizedNamespaceDefinition),
        p2p: (u32, &PartyToParticipant),
        new_member: Option<(&str, &str)>,
        records: Option<&OwnerRecords>,
    ) -> Result {
        let head = SynchronizerHead {
            config,
            synchronizer_id: utils::get_synchronizer_id(config).await?,
        };
        let kicked_participant = self.kicked_participant.as_ref().map(ToString::to_string);
        check_against_head(
            &head,
            self.kind,
            dec_party_id,
            dns,
            p2p,
            new_member,
            Departing {
                participant: kicked_participant.as_deref(),
                owner_key: self.kicked_owner_key.as_deref(),
                records,
            },
        )
        .await
    }

    /// This node's records of who holds the party's owner keys. Its own
    /// namespace stands in for its own host entry, which no peer reports.
    async fn owner_records(
        &self,
        storage: &SqlitePool,
        dec_party_id: &CantonId,
        own_namespace: Option<&Fingerprint>,
    ) -> Result<OwnerRecords> {
        let mut host_keys: BTreeMap<String, String> = storage
            .get_dec_party_participants(dec_party_id)
            .await?
            .into_iter()
            .filter_map(|row| row.owner_key.map(|key| (row.participant_uid, key)))
            .collect();
        if let Some(own_namespace) = own_namespace {
            host_keys.insert(self.self_id.to_string(), own_namespace.to_string());
        }
        let claims = storage
            .get_dec_party_owners(dec_party_id)
            .await?
            .into_iter()
            .filter_map(|row| row.participant_uid.map(|uid| (row.owner_key, uid)))
            .collect();
        Ok(OwnerRecords { host_keys, claims })
    }

    /// The new member's `(namespace, daml)` fingerprints, when this node is
    /// the new member. Only it holds its key bundle for the run, and it must.
    async fn new_member_fingerprints(
        &self,
        storage: &SqlitePool,
        instance_name: &str,
    ) -> Result<Option<(Fingerprint, Fingerprint)>> {
        let Some(new_member) = &self.new_participant else {
            return Ok(None);
        };
        if new_member != &self.self_id {
            return Ok(None);
        }
        let payload = storage
            .read_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&new_member.to_string()),
            )
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "this node is the new member but its public keys are not on the \
                     {instance_name} run, so the added owner and key cannot be pinned to it"
                )
            })?;
        match decode_keys_payload(&payload)?.as_slice() {
            [namespace, daml] => Ok(Some((
                utils::compute_fingerprint(namespace),
                utils::compute_fingerprint(daml),
            ))),
            keys => anyhow::bail!(
                "expected 2 public keys for new member {new_member}, found {count}",
                count = keys.len()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::topology_mapping;

    use super::*;
    use crate::workflow::validation::test_support::*;

    #[tokio::test]
    async fn rejects_proposals_for_another_party() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mut expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.dec_party_id = Some(canton_id("ours", 7)?);
        let their_party = canton_id("theirs", 8)?;

        let dns = encode_proposal(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            DecentralizedNamespaceDefinition {
                decentralized_namespace: their_party.namespace.to_hex(),
                threshold: 2,
                owners: vec!["one".to_string(), "two".to_string()],
            },
        ));
        let p2p_payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &their_party.to_string(),
            &[a, b],
            2,
        )));

        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        assert!(
            expectations
                .check_party_proposals(&NodeConfig::default(), &pool, "run", &dns, &p2p_payload)
                .await
                .is_err()
        );
        Ok(())
    }

    /// Canton reads the owners as a set, so a repeated namespace keeps the
    /// length the invitation expects while shrinking the real owner set — and
    /// the threshold then stands for fewer distinct signatures than agreed.
    #[tokio::test]
    async fn rejects_duplicate_dns_owners() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let party = canton_id("dec", 7)?;
        let mut expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.dec_party_id = Some(party.clone());

        let dns = encode_proposal(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            DecentralizedNamespaceDefinition {
                decentralized_namespace: party.namespace.to_hex(),
                threshold: 2,
                // Two entries, one distinct owner.
                owners: vec!["same".to_string(), "same".to_string()],
            },
        ));
        let p2p_payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &party.to_string(),
            &[a, b],
            2,
        )));

        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        let error = expectations
            .check_party_proposals(&NodeConfig::default(), &pool, "run", &dns, &p2p_payload)
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("repeats an owner"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn the_new_member_refuses_without_its_own_key_bundle(pool: SqlitePool) -> Result {
        let (a, new_member) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mut expectations = expectations(vec![a], new_member.clone());
        expectations.kind = WorkflowKind::AddParty;
        expectations.new_participant = Some(new_member);

        let error = match expectations.new_member_fingerprints(&pool, "run").await {
            Ok(found) => anyhow::bail!("expected a refusal, got {found:?}"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("this node is the new member"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn an_existing_member_has_no_new_member_bundle_to_pin(pool: SqlitePool) -> Result {
        let (a, new_member) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.kind = WorkflowKind::AddParty;
        expectations.new_participant = Some(new_member);

        assert_eq!(
            expectations.new_member_fingerprints(&pool, "run").await?,
            None
        );
        Ok(())
    }

    /// The owner set may outnumber the members: a member can own the
    /// namespace without hosting the party.
    #[test]
    fn a_party_dns_may_list_more_owners_than_members() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let party = canton_id("dec", 7)?;
        let expectations = expectations(vec![a.clone(), b], a);
        expectations.check_party_dns_shape(
            &party,
            &DecentralizedNamespaceDefinition {
                decentralized_namespace: party.namespace.to_hex(),
                threshold: 2,
                owners: owners(&["a", "b", "c"]),
            },
        )
    }

    /// Without a named owner key, nothing ties the owners a kick keeps to the
    /// members it keeps, so the peer counts them as before keys were named.
    #[test]
    fn a_kick_without_an_owner_key_keeps_one_owner_per_member() -> Result {
        let p1 = canton_id("p1", 1)?;
        let party = canton_id("dec", 7)?;
        let dns = |list: &[&str]| DecentralizedNamespaceDefinition {
            decentralized_namespace: party.namespace.to_hex(),
            threshold: 1,
            owners: owners(list),
        };
        let mut kick = PeerExpectations {
            kind: WorkflowKind::Kick,
            threshold: Some(1),
            ..expectations(vec![p1.clone()], p1)
        };
        kick.check_party_dns_shape(&party, &dns(&["a"]))?;
        let error = match kick.check_party_dns_shape(&party, &dns(&["a", "b"])) {
            Ok(()) => anyhow::bail!("two owners for one member must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("names 1 members and no owner key"),
            "unexpected error: {error}"
        );
        kick.kicked_owner_key = Some("c".to_string());
        kick.check_party_dns_shape(&party, &dns(&["a", "b"]))
    }
}
