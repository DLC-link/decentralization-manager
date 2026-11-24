use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToKeyMapping, PartyToParticipant,
        SignedTopologyTransaction, TopologyMapping, TopologyTransaction, enums,
        party_to_participant::{HostingParticipant, hosting_participant},
        topology_mapping,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message;
use sqlx::SqlitePool;

use super::PeerExpectations;
use crate::{
    canton_id::CantonId,
    db::MIGRATOR,
    error::Result,
    server::WorkflowKind,
    signing::{
        custody::SoftwareKeys,
        fake_admin_api::{FakeAdminApi, TestNode, VaultKey},
    },
    utils,
    workflow::{
        onboarding::{
            steps::generate_keys::encode_keys_payload,
            steps::proposals::create::compute_decentralized_namespace,
        },
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
    },
};

const RUN: &str = "party-creation";

fn self_id() -> Result<CantonId> {
    CantonId::parse("node1::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
}

fn peer_id() -> Result<CantonId> {
    CantonId::parse("node2::1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
}

/// A namespace key, as the vault reports one.
fn namespace_key(seed: u8) -> SigningPublicKey {
    SigningPublicKey {
        usage: vec![SigningKeyUsage::Namespace as i32],
        ..VaultKey::software("namespace", seed).public_key
    }
}

fn daml_key(seed: u8) -> SigningPublicKey {
    VaultKey::software("daml", seed).public_key
}

/// The invitation node1 accepted: a party with prefix `party` for node1
/// and node2, at threshold 2.
fn onboarding() -> Result<PeerExpectations> {
    Ok(PeerExpectations {
        kind: WorkflowKind::Onboarding,
        contract_intents: Vec::new(),
        members: [self_id()?, peer_id()?].into_iter().collect(),
        dec_party_id: None,
        prefix: Some("party".to_string()),
        threshold: Some(2),
        confirmation_threshold: None,
        kicked_participant: None,
        kicked_owner_key: None,
        new_participant: None,
        dar_filenames: Vec::new(),
        dar_hashes: Vec::new(),
        rotated_key: None,
        coordinator_id: peer_id()?,
        self_id: self_id()?,
    })
}

/// Record node1's keys on its onboarding run, as GenerateKeys does.
async fn record_own_keys(db: &SqlitePool) -> Result {
    sqlx::query(
        "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES (?1, 'Onboarding', 'Peer', 'inprogress', 'SignDns',
                       0, 1, '{}', '[]', '[]', 0, 0)",
    )
    .bind(RUN)
    .execute(db)
    .await?;
    db.write_artifact(
        RUN,
        artifact_kinds::PEER_PUBLIC_KEYS,
        Some(&self_id()?.to_string()),
        &encode_keys_payload(&namespace_key(1), &daml_key(3)),
    )
    .await
}

/// The namespace the invitation describes: node1's and node2's
/// namespace keys as owners, at threshold 2.
fn proposed_namespace() -> DecentralizedNamespaceDefinition {
    let owners: Vec<String> = [namespace_key(1), namespace_key(2)]
        .iter()
        .map(|key| utils::compute_fingerprint(key).to_string())
        .collect();
    DecentralizedNamespaceDefinition {
        decentralized_namespace: compute_decentralized_namespace(&owners.iter().cloned().collect()),
        threshold: 2,
        owners,
    }
}

fn proposed_p2p() -> Result<PartyToParticipant> {
    let host = |id: CantonId| HostingParticipant {
        participant_uid: id.to_string(),
        permission: enums::ParticipantPermission::Confirmation as i32,
        onboarding: None::<hosting_participant::Onboarding>,
    };
    Ok(PartyToParticipant {
        party: format!(
            "party::{namespace}",
            namespace = proposed_namespace().decentralized_namespace
        ),
        threshold: 2,
        participants: vec![host(self_id()?), host(peer_id()?)],
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: vec![daml_key(3), daml_key(4)],
            threshold: 2,
        }),
    })
}

/// The coordinator's proposal at serial 1, as it ships it.
fn payload(mapping: topology_mapping::Mapping) -> Vec<u8> {
    payload_at(1, mapping)
}

fn payload_at(serial: u32, mapping: topology_mapping::Mapping) -> Vec<u8> {
    let transaction = TopologyTransaction {
        operation: enums::TopologyChangeOp::AddReplace as i32,
        serial,
        mapping: Some(TopologyMapping {
            mapping: Some(mapping),
        }),
    };
    utils::encode_length_prefixed_message(&SignedTopologyTransaction {
        transaction: UntypedVersionedMessage {
            version: 30,
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                transaction.encode_to_vec(),
            )),
        }
        .encode_to_vec(),
        ..Default::default()
    })
}

async fn node(fake: &FakeAdminApi) -> Result<TestNode> {
    fake.node(SoftwareKeys::Stored, true).await
}

/// An onboarding peer checks a proposal against its invitation only. A
/// namespace already on the chain with other content belongs to an
/// existing party, so the peer must not sign a change to it. A retried
/// peer may sign the proposal that is already in force.
#[sqlx::test(migrator = "MIGRATOR")]
async fn an_onboarding_peer_signs_a_namespace_only_while_it_does_not_exist(
    db: SqlitePool,
) -> Result {
    record_own_keys(&db).await?;
    let expectations = onboarding()?;
    let proposal = payload(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
        proposed_namespace(),
    ));

    let fake = FakeAdminApi::start(vec![]).await?;
    let new = node(&fake).await?;
    let namespace = expectations
        .check_onboarding_dns(&new.config, &db, RUN, &proposal)
        .await?;
    assert_eq!(namespace, proposed_namespace().decentralized_namespace);
    fake.publish_namespace(1, proposed_namespace());
    expectations
        .check_onboarding_dns(&new.config, &db, RUN, &proposal)
        .await?;

    let existing = FakeAdminApi::start(vec![]).await?;
    existing.publish_namespace(
        1,
        DecentralizedNamespaceDefinition {
            threshold: 1,
            ..proposed_namespace()
        },
    );
    let error = match expectations
        .check_onboarding_dns(&node(&existing).await?.config, &db, RUN, &proposal)
        .await
    {
        Ok(namespace) => anyhow::bail!("signed a change to the existing {namespace}"),
        Err(e) => e.to_string(),
    };
    assert!(error.contains("The DNS mapping already exists"), "{error}");
    Ok(())
}

/// The same rule holds for the party mapping. A coordinator could
/// otherwise reuse a known namespace and rewrite an existing party's
/// hosts and keys through an onboarding run.
#[sqlx::test(migrator = "MIGRATOR")]
async fn an_onboarding_peer_refuses_a_party_mapping_that_exists(db: SqlitePool) -> Result {
    record_own_keys(&db).await?;
    let expectations = onboarding()?;
    let namespace = proposed_namespace().decentralized_namespace;
    let proposal = payload(topology_mapping::Mapping::PartyToParticipant(
        proposed_p2p()?
    ));

    let fake = FakeAdminApi::start(vec![]).await?;
    expectations
        .check_onboarding_p2p(
            &node(&fake).await?.config,
            &db,
            RUN,
            &proposal,
            Some(&namespace),
        )
        .await?;

    let existing = FakeAdminApi::start(vec![]).await?;
    existing.publish_p2p(
        1,
        PartyToParticipant {
            threshold: 1,
            ..proposed_p2p()?
        },
    );
    let error = match expectations
        .check_onboarding_p2p(
            &node(&existing).await?.config,
            &db,
            RUN,
            &proposal,
            Some(&namespace),
        )
        .await
    {
        Ok(()) => anyhow::bail!("signed a change to an existing party"),
        Err(e) => e.to_string(),
    };
    assert!(error.contains("The P2P mapping already exists"), "{error}");
    Ok(())
}

/// A party whose Daml keys sit in a legacy `PartyToKeyMapping`, hosted
/// by node1 and node2. The mapping still holds key 9 of a member that
/// left. Each node's records name key 3 as node1's and key 4 as node2's.
async fn legacy_party(fake: &FakeAdminApi, db: &SqlitePool) -> Result<CantonId> {
    let party = CantonId::parse(&proposed_p2p()?.party)?;
    fake.publish_namespace(
        2,
        DecentralizedNamespaceDefinition {
            threshold: 1,
            ..proposed_namespace()
        },
    );
    fake.publish_p2p(
        5,
        PartyToParticipant {
            threshold: 1,
            party_signing_keys: None,
            ..proposed_p2p()?
        },
    );
    fake.publish_party_to_key(
        1,
        PartyToKeyMapping {
            party: party.to_string(),
            threshold: 1,
            signing_keys: vec![daml_key(3), daml_key(4), daml_key(9)],
        },
    );
    for (member, namespace, daml) in [(self_id()?, 1, 3), (peer_id()?, 2, 4)] {
        db.write_identity(
            &party,
            identity_kinds::PEER_PUBLIC_KEYS,
            &member.to_string(),
            &encode_keys_payload(&namespace_key(namespace), &daml_key(daml)),
        )
        .await?;
    }
    Ok(party)
}

/// A legacy mapping lets a proposal drop any of its keys, so the chain
/// alone cannot tell a departed member's key from a current one. A
/// change-threshold that moves the keys inline must keep exactly the
/// key each host has on record. Keeping the departed key in place of
/// node2's is refused.
#[sqlx::test(migrator = "MIGRATOR")]
async fn a_legacy_change_threshold_keeps_each_hosts_recorded_key(db: SqlitePool) -> Result {
    let fake = FakeAdminApi::start(vec![]).await?;
    let node = node(&fake).await?;
    let party = legacy_party(&fake, &db).await?;
    let expectations = PeerExpectations {
        kind: WorkflowKind::ChangeThreshold,
        dec_party_id: Some(party),
        prefix: None,
        coordinator_id: peer_id()?,
        ..onboarding()?
    };
    let dns = payload_at(
        3,
        topology_mapping::Mapping::DecentralizedNamespaceDefinition(proposed_namespace()),
    );
    let p2p_keeping = |keys: Vec<SigningPublicKey>| -> Result<Vec<u8>> {
        Ok(payload_at(
            6,
            topology_mapping::Mapping::PartyToParticipant(PartyToParticipant {
                party_signing_keys: Some(SigningKeysWithThreshold { keys, threshold: 2 }),
                ..proposed_p2p()?
            }),
        ))
    };

    expectations
        .check_party_proposals(
            &node.config,
            &db,
            "change-threshold",
            &dns,
            &p2p_keeping(vec![daml_key(3), daml_key(4)])?,
        )
        .await?;

    let error = match expectations
        .check_party_proposals(
            &node.config,
            &db,
            "change-threshold",
            &dns,
            &p2p_keeping(vec![daml_key(3), daml_key(9)])?,
        )
        .await
    {
        Ok(()) => anyhow::bail!("signed a proposal that drops node2's key"),
        Err(e) => e.to_string(),
    };
    assert!(
        error.contains(&format!(
            "drops {fingerprint}, which this node has on record as {peer}'s Daml key",
            fingerprint = utils::compute_fingerprint(&daml_key(4)),
            peer = peer_id()?
        )),
        "{error}"
    );
    Ok(())
}
