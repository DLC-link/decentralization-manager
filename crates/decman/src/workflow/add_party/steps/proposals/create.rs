use std::collections::BTreeMap;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping, enums,
        party_to_participant::{HostingParticipant, hosting_participant},
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ForceFlag, authorize_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    utils,
    workflow::{
        add_party::AddPartyConfig,
        onboarding::steps::proposals::create::decode_keys_payload,
        proposal_store,
        signing_keys::{adopt_legacy_signing_keys, own_namespace_key},
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
        topology,
    },
};

/// Coordinator step: build and propose the updated topology for the add.
///
/// A member whose namespace already owns the party is a former host being
/// hosted again: the namespace and the party's signing keys are left as they
/// are, only the `PartyToParticipant` grows, and `ADD_PARTY_REHOST` tells the
/// submit step to leave the namespace alone. The peers still sign a DNS
/// transaction in that case, the one already in force, so their signing round
/// is the same as for any add.
///
/// Creates:
/// - new `DecentralizedNamespaceDefinition`: same namespace hash, existing
///   owners + the new member's namespace fingerprint, the new threshold —
///   persisted as `ADD_PARTY_DNS_PROPOSAL` (+ `ADD_PARTY_NEW_NAMESPACE_DEF`
///   for submit's propagation wait)
/// - new `PartyToParticipant`: existing participants + the new member with
///   `Confirmation` permission and the **Onboarding marker** set (suspends
///   the party on the new member until the ACS import lands and the flag is
///   cleared), party signing keys merged with the new member's Daml key —
///   persisted as `ADD_PARTY_P2P_PROPOSAL`
pub async fn create_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Creating add-party proposals...");

    let namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::ADD_PARTY_NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("ADD_PARTY_NAMESPACE_DEF artifact missing — did ExportState run?")
        })?;
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    let new_member_id = add_party_config.new_participant_id.to_string();
    let keys_payload = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&new_member_id),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "PEER_PUBLIC_KEYS artifact missing for new member {new_member_id} — \
                 did GenerateNewMemberKeys run?"
            )
        })?;
    let keys = decode_keys_payload(&keys_payload)?;
    if keys.len() != 2 {
        anyhow::bail!(
            "Expected exactly 2 keys from new member {new_member_id}, found {count}",
            count = keys.len()
        );
    }
    let new_namespace_fingerprint = utils::compute_fingerprint(&keys[0]);
    let new_daml_key = keys[1].clone();
    let new_daml_fingerprint = utils::compute_fingerprint(&new_daml_key);
    tracing::info!(
        "New member namespace fingerprint: {new_namespace_fingerprint}, \
         Daml key fingerprint: {new_daml_fingerprint}"
    );

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let party_id = &add_party_config.decentralized_party_id;
    // The serials the proposals are pinned to, read with the mappings they
    // are built on so the two cannot disagree.
    let head = proposal_store::fetch_party_head(config, &synchronizer_id, party_id).await?;
    head.ensure_namespace_is(&current_namespace_def)?;
    let current_p2p = head.p2p.clone();

    let current_members: Vec<String> = current_p2p
        .participants
        .iter()
        .map(|p| p.participant_uid.clone())
        .collect();
    let own_namespace = own_namespace_key(config, party_id, &synchronizer_id)
        .await?
        .map(|key| utils::compute_fingerprint(&key));
    let rehost = is_former_host(
        storage,
        party_id,
        &new_member_id,
        &current_members,
        (
            &config.participant_id().to_string(),
            own_namespace.as_deref(),
        ),
        &current_namespace_def,
        &new_namespace_fingerprint,
    )
    .await?;

    let new_threshold = add_party_config.new_threshold;
    let new_namespace_def = if rehost {
        if new_threshold != current_namespace_def.threshold {
            anyhow::bail!(
                "Re-hosting {new_member_id} leaves the namespace as it is, so its threshold \
                 stays {current}; got new_threshold {new_threshold}. Change it afterwards with \
                 change-threshold",
                current = current_namespace_def.threshold
            );
        }
        tracing::info!(
            "{new_member_id} already owns namespace {ns}; hosting it again without touching \
             the namespace",
            ns = current_namespace_def.decentralized_namespace
        );
        current_namespace_def.clone()
    } else {
        let mut new_owners = current_namespace_def.owners.clone();
        new_owners.push(new_namespace_fingerprint.clone());
        new_owners.sort();
        DecentralizedNamespaceDefinition {
            decentralized_namespace: current_namespace_def.decentralized_namespace.clone(),
            threshold: new_threshold,
            owners: new_owners,
        }
    };

    tracing::info!(
        "Current P2P mapping has {count} participant(s)",
        count = current_p2p.participants.len()
    );

    let mut new_participants = current_p2p.participants.clone();
    new_participants.push(HostingParticipant {
        participant_uid: new_member_id.clone(),
        permission: enums::ParticipantPermission::Confirmation as i32,
        // The Onboarding marker keeps the party suspended on the new member
        // until the ACS import lands and the flag-clearing round removes it.
        onboarding: Some(hosting_participant::Onboarding {}),
    });

    // A party onboarded before Canton 3.4 carries no inline signing keys: they
    // sit in the deprecated PartyToKeyMapping instead. Merging into that empty
    // set proposed a party whose only signing key was the new member's, which
    // Canton refuses outright above a threshold of one.
    //
    // Fewer keys than members, rather than none, because a pre-1.10.0 add left
    // parties holding exactly that: one inline key, the joiner's, and the rest
    // still in the legacy mapping. Adopting only on an empty set left those
    // parties unable to change membership at all.
    let current_signing_keys = current_p2p
        .party_signing_keys
        .map(|sk| sk.keys)
        .unwrap_or_default();
    let mut signing_keys = if current_signing_keys.len() < current_p2p.participants.len() {
        let members: Vec<String> = current_p2p
            .participants
            .iter()
            .map(|p| p.participant_uid.clone())
            .collect();
        adopt_legacy_signing_keys(
            config,
            storage,
            &synchronizer_id,
            party_id,
            &members,
            &current_signing_keys,
        )
        .await?
    } else {
        current_signing_keys
    };

    // Merge the new member's Daml key into the party signing keys, deduped by
    // fingerprint so a retried run can't double-add it.
    if !signing_keys
        .iter()
        .any(|k| utils::compute_fingerprint(k) == new_daml_fingerprint)
    {
        signing_keys.push(new_daml_key);
    }

    if signing_keys.len() != new_participants.len() {
        anyhow::bail!(
            "Add-party would leave the party with {keys} signing key(s) for {members} \
             member(s). Every member contributes exactly one, so the key set does not match \
             the membership and the peers would refuse the proposal",
            keys = signing_keys.len(),
            members = new_participants.len()
        );
    }

    let new_p2p = PartyToParticipant {
        party: party_id.to_string(),
        threshold: new_threshold.try_into()?,
        participants: new_participants,
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: signing_keys,
            threshold: new_threshold.try_into()?,
        }),
    };

    // Publishing the P2P proposal before submit (below) is safe only while it
    // adds the new member as a host, so check that before anything is signed.
    if !adds_host(&head.p2p, &new_p2p, &new_member_id) {
        anyhow::bail!(
            "The add-party P2P proposal does not add {new_member_id} as a host, so this node's \
             signature alone could put it in force"
        );
    }

    // Signed without being published, so a member that never signs leaves the
    // party unchanged (#448). A re-host leaves the namespace as it is, so no
    // DNS is signed for it.
    tracing::info!("Signing add-party proposals...");
    let signed = proposal_store::sign_party_proposals(
        config,
        &synchronizer_id,
        instance_name,
        party_id,
        &head,
        (!rehost).then(|| new_namespace_def.clone()),
        new_p2p,
        topology::party_proposal_force_flags(),
    )
    .await?;
    let dns_transaction = match signed.dns {
        Some(dns) => dns,
        None => {
            topology::fetch_signed_namespace_definition(
                config,
                &synchronizer_id,
                &new_namespace_def.decentralized_namespace,
            )
            .await?
        }
    };
    let p2p_transaction = signed.p2p;

    // Published before submit on purpose. The new member disconnects before
    // the mapping that hosts it is authorized (#469), and its ACS import needs
    // that mapping in its own synchronizer store, which holds only what was
    // sequenced. Adding a host needs that host's own signature, so the
    // proposal stays pending until the new member signs. The DNS stays
    // unpublished until submit, unless the signing fell back to the
    // synchronizer above threshold 1; Canton then ignores this repeat.
    tracing::info!("Publishing the add-party P2P proposal for the new member's store...");
    TopologyManagerWriteServiceClient::new(config.admin_channel().await?)
        .add_transactions(tonic::Request::new(topology::add_transactions_request(
            &synchronizer_id,
            p2p_transaction.clone(),
            topology::party_proposal_force_flags(),
        )))
        .await?;

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_DNS_PROPOSAL,
            None,
            &utils::encode_length_prefixed_message(&dns_transaction),
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_P2P_PROPOSAL,
            None,
            &utils::encode_length_prefixed_message(&p2p_transaction),
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_NEW_NAMESPACE_DEF,
            None,
            &utils::encode_length_prefixed_message(&new_namespace_def),
        )
        .await?;
    if rehost {
        storage
            .write_artifact(instance_name, artifact_kinds::ADD_PARTY_REHOST, None, b"1")
            .await?;
    }

    tracing::info!("Add-party proposals created and saved successfully");
    Ok(())
}

/// Whether the member being added already owns the party's namespace because
/// it hosted the party before and only its hosting entry was removed.
///
/// The danger is one participant claiming another's namespace key, which would
/// let either act for the other. So the add is refused when this node can tie
/// `namespace_fingerprint` to anyone other than the member being added, and
/// also when it cannot tell which key some current member holds, since that
/// unknown key might be this one.
///
/// An owner key that no member claims, with every member accounted for, is a
/// former host's, left behind when its `PartyToParticipant` entry went and the
/// namespace stayed. Hosting that member again is the case this function exists
/// to allow, and the re-host path leaves the owner set exactly as it is.
///
/// `own` is this node's participant id and the namespace fingerprint its vault
/// holds for the party. No peer reports this node's key back to it, so the vault
/// is the only source for that one member.
async fn is_former_host(
    storage: &SqlitePool,
    party_id: &crate::canton_id::CantonId,
    new_member_id: &str,
    current_members: &[String],
    own: (&str, Option<&str>),
    namespace_def: &DecentralizedNamespaceDefinition,
    namespace_fingerprint: &str,
) -> Result<bool> {
    if !namespace_def
        .owners
        .iter()
        .any(|o| o == namespace_fingerprint)
    {
        return Ok(false);
    }

    let recorded = recorded_namespace_keys(storage, party_id).await?;
    // A record ties the key to whoever it names, whether or not that member
    // still hosts the party.
    if let Some((owner, _)) = recorded
        .iter()
        .find(|(member, key)| *key == namespace_fingerprint && member.as_str() != new_member_id)
    {
        anyhow::bail!(
            "Namespace fingerprint {namespace_fingerprint} is already a DNS owner and this node \
             attributes it to {owner}, not {new_member_id} — the new member appears to reuse an \
             existing member's namespace key"
        );
    }

    let mut unattributed = Vec::new();
    for member in current_members {
        let cached = storage
            .get_dec_party_participant_owner_key(party_id, member)
            .await?;
        let claim = match (recorded.get(member), cached) {
            (Some(key), Some(cached)) => {
                if *key != cached {
                    tracing::warn!(
                        "{member} reports namespace key {cached} for {party_id} but this node \
                         recorded {key}; taking the recorded one"
                    );
                }
                Some(key.clone())
            }
            (Some(key), None) => Some(key.clone()),
            (None, Some(cached)) => Some(cached),
            (None, None) if member == own.0 => own.1.map(str::to_string),
            (None, None) => None,
        };
        match claim {
            Some(key) if key == namespace_fingerprint => anyhow::bail!(
                "Namespace fingerprint {namespace_fingerprint} is already a DNS owner and this \
                 node attributes it to {member}, not {new_member_id} — the new member appears to \
                 reuse an existing member's namespace key"
            ),
            Some(_) => {}
            None => unattributed.push(member.as_str()),
        }
    }
    if !unattributed.is_empty() {
        anyhow::bail!(
            "Namespace fingerprint {namespace_fingerprint} is already a DNS owner, and this node \
             does not know the namespace key of {members} for {party_id}, so it cannot rule out \
             that {new_member_id} is reusing one of them. Refresh the decentralized parties so \
             each member reports its key, then retry",
            members = unattributed.join(", ")
        );
    }

    if recorded.get(new_member_id).map(String::as_str) == Some(namespace_fingerprint) {
        return Ok(true);
    }
    tracing::warn!(
        "Namespace fingerprint {namespace_fingerprint} owns {party_id} and no member of the \
         party claims it, so it belongs to a former host; hosting {new_member_id} again under \
         it. This node holds no identity record for {new_member_id}, which is expected when \
         another node coordinated the run that first added it."
    );
    Ok(true)
}

/// Whether `proposed` hosts `new_member` and `current` does not.
///
/// Canton requires an added host's own signature on the mapping that adds it.
/// A proposal that adds `new_member` therefore cannot take effect before
/// `new_member` signs, whatever the namespace threshold.
fn adds_host(
    current: &PartyToParticipant,
    proposed: &PartyToParticipant,
    new_member: &str,
) -> bool {
    let hosts = |mapping: &PartyToParticipant| {
        mapping
            .participants
            .iter()
            .any(|host| host.participant_uid == new_member)
    };
    hosts(proposed) && !hosts(current)
}

/// The namespace fingerprint this node recorded for each member, from the
/// identity rows written by the run that added it.
///
/// Rows exist for every member on the node that coordinated the onboarding,
/// and for itself alone on each peer, so an absent row says nothing about who
/// owns a key. A row whose first key is not a namespace key records no
/// namespace at all and is left out: older builds and chain backfills wrote the
/// Daml key there, and counting that as an answer would mark a member as
/// accounted for without knowing its namespace.
async fn recorded_namespace_keys(
    storage: &SqlitePool,
    party_id: &crate::canton_id::CantonId,
) -> Result<BTreeMap<String, String>> {
    let mut recorded = BTreeMap::new();
    for (member, payload) in storage
        .list_identity(party_id, identity_kinds::PEER_PUBLIC_KEYS)
        .await?
    {
        match decode_keys_payload(&payload) {
            Ok(keys) => {
                if let Some(key) = keys
                    .first()
                    .filter(|key| key.usage.contains(&(SigningKeyUsage::Namespace as i32)))
                {
                    recorded.insert(member, utils::compute_fingerprint(key));
                }
            }
            Err(e) => {
                tracing::warn!("PEER_PUBLIC_KEYS for {member} on {party_id} will not decode: {e:#}")
            }
        }
    }
    Ok(recorded)
}

/// Build an `AuthorizeRequest` proposing `mapping` against the synchronizer
/// store. Serial 0 lets Canton pick the next serial for the existing mapping;
/// `AllowUnvalidatedSigningKeys` is needed because the new member's keys may
/// not have reached the synchronizer store yet when the coordinator proposes
/// (same reason onboarding's proposals carry it).
pub(crate) fn proposal_request(
    synchronizer_id: &str,
    mapping: topology_mapping::Mapping,
) -> AuthorizeRequest {
    AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(mapping),
                })),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![ForceFlag::AllowUnvalidatedSigningKeys as i32],
        signed_by: vec![],
        store: Some(topology::synchronizer_store_id(synchronizer_id)),
        wait_to_become_effective: None,
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        SigningKeyUsage, SigningPublicKey,
    };
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        canton_id::CantonId,
        db::{
            MIGRATOR,
            rows::{DecPartyParticipantRow, DecPartyRow},
            schema::{Commitable, SchemaWrite},
        },
        workflow::onboarding::steps::generate_keys::encode_keys_payload,
    };

    fn key(seed: u8, usage: SigningKeyUsage) -> SigningPublicKey {
        SigningPublicKey {
            public_key: vec![seed; 32],
            usage: vec![usage as i32],
            ..Default::default()
        }
    }

    fn party() -> CantonId {
        CantonId::parse(&format!("party::{}", "1220".to_owned() + &"ab".repeat(32)))
            .expect("valid party id")
    }

    /// This node's participant id in these tests, and a vault that names none of
    /// the party's owners, as on a node that is not itself a member.
    const NO_VAULT: (&str, Option<&str>) = ("p1::1220cc", None);

    /// Put `member` in the party's participant cache with `owner_key`, the way
    /// `resolve_owner_keys_from_peers` does after a peer answers. `None` is a
    /// member whose answer has not arrived yet.
    async fn seed_party_with_owner_key(
        pool: &SqlitePool,
        party_id: &CantonId,
        member: &str,
        owner_key: Option<&str>,
    ) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party_id.to_string(),
            prefix: "party".to_string(),
            threshold: 1,
            updated_at: 0,
            my_owner_key: None,
        })
        .await?;
        tx.replace_dec_party_participants(
            party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id.to_string(),
                participant_uid: member.to_string(),
                permission: "confirmation".to_string(),
                owner_key: owner_key.map(str::to_string),
                signing_key: None,
            }],
        )
        .await?;
        Commitable::commit(tx).await
    }

    async fn record_keys(
        pool: &SqlitePool,
        party_id: &CantonId,
        member: &str,
        first: &SigningPublicKey,
    ) -> Result {
        pool.write_identity(
            party_id,
            identity_kinds::PEER_PUBLIC_KEYS,
            member,
            &encode_keys_payload(first, &key(2, SigningKeyUsage::Protocol)),
        )
        .await
    }

    fn namespace_def(owners: &[String]) -> DecentralizedNamespaceDefinition {
        DecentralizedNamespaceDefinition {
            decentralized_namespace: "ns".to_string(),
            threshold: 1,
            owners: owners.to_vec(),
        }
    }

    fn assert_refused(err: anyhow::Error, needle: &str) {
        assert!(format!("{err}").contains(needle), "{err}");
    }

    fn hosting(uids: &[&str]) -> PartyToParticipant {
        PartyToParticipant {
            participants: uids
                .iter()
                .map(|uid| HostingParticipant {
                    participant_uid: (*uid).to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// The P2P proposal is published before submit only because it adds the
    /// new member, whose own signature Canton then requires.
    #[test]
    fn only_a_proposal_that_adds_the_new_member_counts_as_adding_a_host() {
        let current = hosting(&["p1::ns", "p2::ns"]);
        assert!(adds_host(
            &current,
            &hosting(&["p1::ns", "p2::ns", "p3::ns"]),
            "p3::ns"
        ));
        assert!(!adds_host(&current, &current, "p3::ns"));
        assert!(!adds_host(
            &hosting(&["p1::ns", "p3::ns"]),
            &hosting(&["p1::ns", "p3::ns"]),
            "p3::ns"
        ));
        assert!(!adds_host(
            &current,
            &hosting(&["p1::ns", "p2::ns", "p4::ns"]),
            "p3::ns"
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_namespace_that_is_not_an_owner_is_a_plain_add(pool: SqlitePool) -> Result {
        let fingerprint = utils::compute_fingerprint(&key(1, SigningKeyUsage::Namespace));
        let def = namespace_def(&["other".to_string()]);
        let rehost = is_former_host(
            &pool,
            &party(),
            "p3::1220aa",
            &[],
            NO_VAULT,
            &def,
            &fingerprint,
        )
        .await?;
        assert!(!rehost);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_this_node_recorded_for_the_member_is_a_rehost(pool: SqlitePool) -> Result {
        let namespace = key(1, SigningKeyUsage::Namespace);
        let fingerprint = utils::compute_fingerprint(&namespace);
        let party = party();
        record_keys(&pool, &party, "p3::1220aa", &namespace).await?;
        let def = namespace_def(&[fingerprint.clone(), "other".to_string()]);
        let rehost = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &[],
            NO_VAULT,
            &def,
            &fingerprint,
        )
        .await?;
        assert!(rehost);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_recorded_for_another_member_is_refused(pool: SqlitePool) -> Result {
        let namespace = key(1, SigningKeyUsage::Namespace);
        let fingerprint = utils::compute_fingerprint(&namespace);
        let party = party();
        record_keys(&pool, &party, "p2::1220bb", &namespace).await?;
        let def = namespace_def(std::slice::from_ref(&fingerprint));
        let members = ["p2::1220bb".to_string()];
        let err = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &members,
            NO_VAULT,
            &def,
            &fingerprint,
        )
        .await
        .expect_err("an owner key another member holds must be refused");
        assert_refused(err, "attributes it to p2::1220bb");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_a_current_member_claims_in_the_cache_is_refused(pool: SqlitePool) -> Result {
        let fingerprint = utils::compute_fingerprint(&key(1, SigningKeyUsage::Namespace));
        let party = party();
        seed_party_with_owner_key(&pool, &party, "p2::1220bb", Some(&fingerprint)).await?;
        let def = namespace_def(std::slice::from_ref(&fingerprint));
        let members = ["p2::1220bb".to_string()];
        let err = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &members,
            NO_VAULT,
            &def,
            &fingerprint,
        )
        .await
        .expect_err("an owner key a current member claims must be refused");
        assert_refused(err, "attributes it to p2::1220bb");
        Ok(())
    }

    /// A record for the new member does not settle it: a current member that
    /// also claims the key would share it with the new one.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_key_also_claimed_by_a_current_member_is_refused(pool: SqlitePool) -> Result {
        let namespace = key(1, SigningKeyUsage::Namespace);
        let fingerprint = utils::compute_fingerprint(&namespace);
        let party = party();
        record_keys(&pool, &party, "p3::1220aa", &namespace).await?;
        seed_party_with_owner_key(&pool, &party, "p2::1220bb", Some(&fingerprint)).await?;
        let def = namespace_def(std::slice::from_ref(&fingerprint));
        let members = ["p2::1220bb".to_string()];
        let err = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &members,
            NO_VAULT,
            &def,
            &fingerprint,
        )
        .await
        .expect_err("a key two participants claim must be refused");
        assert_refused(err, "attributes it to p2::1220bb");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_current_member_with_no_known_key_fails_closed(pool: SqlitePool) -> Result {
        let fingerprint = utils::compute_fingerprint(&key(1, SigningKeyUsage::Namespace));
        let party = party();
        seed_party_with_owner_key(&pool, &party, "p2::1220bb", None).await?;
        let def = namespace_def(std::slice::from_ref(&fingerprint));
        let members = ["p2::1220bb".to_string()];
        let err = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &members,
            NO_VAULT,
            &def,
            &fingerprint,
        )
        .await
        .expect_err("an unattributed current member must not be read as a non-owner");
        assert_refused(err, "does not know the namespace key of p2::1220bb");
        Ok(())
    }

    /// Older builds recorded the Daml key where the namespace key belongs. Such
    /// a row says nothing about the member's namespace.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_record_without_a_namespace_key_does_not_attribute(pool: SqlitePool) -> Result {
        let fingerprint = utils::compute_fingerprint(&key(1, SigningKeyUsage::Namespace));
        let party = party();
        record_keys(
            &pool,
            &party,
            "p2::1220bb",
            &key(3, SigningKeyUsage::Protocol),
        )
        .await?;
        let def = namespace_def(std::slice::from_ref(&fingerprint));
        let members = ["p2::1220bb".to_string()];
        let err = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &members,
            NO_VAULT,
            &def,
            &fingerprint,
        )
        .await
        .expect_err("a Daml key at [0] must not count as the member's namespace");
        assert_refused(err, "does not know the namespace key of p2::1220bb");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn this_nodes_own_key_comes_from_its_vault(pool: SqlitePool) -> Result {
        let fingerprint = utils::compute_fingerprint(&key(1, SigningKeyUsage::Namespace));
        let party = party();
        let def = namespace_def(std::slice::from_ref(&fingerprint));
        let members = ["p1::1220cc".to_string()];
        let err = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &members,
            ("p1::1220cc", Some(fingerprint.as_str())),
            &def,
            &fingerprint,
        )
        .await
        .expect_err("a key this node's own vault holds must be refused");
        assert_refused(err, "attributes it to p1::1220cc");
        Ok(())
    }

    /// The case that blocked re-hosting a former member: the key is an owner,
    /// every current member is accounted for and none claims it, and this node
    /// never recorded it because a different node coordinated the run that first
    /// added that member.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_no_member_claims_is_a_former_hosts_key(pool: SqlitePool) -> Result {
        let fingerprint = utils::compute_fingerprint(&key(1, SigningKeyUsage::Namespace));
        let party = party();
        seed_party_with_owner_key(&pool, &party, "p2::1220bb", Some("someone-elses-key")).await?;
        let def = namespace_def(&[
            fingerprint.clone(),
            "someone-elses-key".to_string(),
            "own-key".to_string(),
        ]);
        let members = ["p1::1220cc".to_string(), "p2::1220bb".to_string()];
        let rehost = is_former_host(
            &pool,
            &party,
            "p3::1220aa",
            &members,
            ("p1::1220cc", Some("own-key")),
            &def,
            &fingerprint,
        )
        .await?;
        assert!(rehost);
        Ok(())
    }
}
