use std::collections::BTreeSet;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{SignedTopologyTransaction, TopologyTransaction, topology_mapping},
    topology::admin::v30::{
        AddTransactionsRequest, ForceFlag, ListAllRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use tonic::transport::Channel;

use super::{
    route::{is_transport_code, is_unreachable},
    sign::PartyHead,
    store::temporary_store_id,
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{topology, topology_version},
};

/// Mapping codes a delegation read excludes, so it returns only the
/// `NamespaceDelegation`s. A participant's namespace also holds its vetting,
/// its key mappings and every local party it hosts, none of which the replay
/// needs. Only codes every Canton 3.x release knows are listed, because an
/// unknown code fails the whole read.
const NOT_DELEGATIONS: [&str; 12] = [
    "dnd", "otk", "dtc", "pdp", "phl", "vtp", "ptp", "dop", "mds", "sds", "sep", "ptk",
];

/// Where a transaction falls in the replay. Delegations go first among
/// transactions that took effect together, roots before the keys they
/// delegate to, and a namespace before the party mapping it authorizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    RootDelegation,
    Delegation,
    Namespace,
    PartyMapping,
}

/// One transaction to replay.
#[derive(Clone, Debug)]
struct Replay {
    /// Effective time as `(seconds, nanos)`; `None` sorts first.
    valid_from: Option<(i64, i32)>,
    rank: Rank,
    serial: u32,
    signed: SignedTopologyTransaction,
}

/// One transaction from the synchronizer's history.
struct HistoryItem {
    valid_from: Option<prost_types::Timestamp>,
    signed: SignedTopologyTransaction,
}

/// Replay the party's history into `store`, and return how many transactions
/// the store accepted.
///
/// The head transactions alone do not do. Without a predecessor in the store,
/// Canton judges a transaction as a creation: a DNS then needs every owner's
/// signature, and a P2P every hosting participant's. The heads usually carry
/// only a quorum. Replaying the whole history in the order it took effect
/// lets each transaction be judged against the state it was signed for.
///
/// A transaction the store refuses is logged and skipped;
/// [`ensure_seeded_to`] decides afterwards whether the heads came out right.
pub(super) async fn seed(
    channel: &Channel,
    synchronizer_id: &str,
    store: &str,
    party_id: &CantonId,
) -> Result<usize> {
    let party_items =
        list_history(channel, synchronizer_id, &party_id.namespace.to_hex(), &[]).await?;
    let (mut replays, namespaces) = party_replays(party_id, party_items)?;
    for namespace in &namespaces {
        let items = list_history(channel, synchronizer_id, namespace, &NOT_DELEGATIONS).await?;
        replays.extend(delegation_replays(namespace, items)?);
    }

    let mut writer = TopologyManagerWriteServiceClient::new(channel.clone());
    let mut accepted = 0;
    for signed in replay_order(replays) {
        match writer
            .add_transactions(tonic::Request::new(AddTransactionsRequest {
                transactions: vec![signed],
                force_changes: replay_force_flags(),
                store: Some(temporary_store_id(store)),
                wait_to_become_effective: None,
            }))
            .await
        {
            Ok(_) => accepted += 1,
            // A lost connection is not a refusal, and must not be read as a
            // history that does not replay.
            Err(status) if is_transport_code(status.code()) => return Err(status.into()),
            // Unexpected, since the synchronizer accepted every one of these.
            // The run can still go on if the heads come out right.
            Err(status) => tracing::warn!(
                store,
                %party_id,
                %status,
                "A temporary store refused a replayed topology transaction"
            ),
        }
    }
    Ok(accepted)
}

/// Force flags for the replay.
///
/// Canton runs its "is this dangerous" checks again on every replayed
/// transaction, relative to this node and its ledger state today rather than
/// to the node that first submitted it. Replaying a former unhosting of this
/// node, for instance, would otherwise trip the active-contracts check. The
/// replay only rebuilds history into a store that is dropped afterwards.
fn replay_force_flags() -> Vec<i32> {
    vec![
        ForceFlag::DisablePartyWithActiveContracts as i32,
        ForceFlag::AllowInsufficientParticipantPermissionForSignatoryParty as i32,
        ForceFlag::AllowInsufficientSignatoryAssigningParticipantsForParty as i32,
        ForceFlag::AllowConfirmingThresholdCannotBeMet as i32,
    ]
}

/// Read every transaction the synchronizer ever held under `namespace`,
/// leaving out mappings with a code in `exclude`.
async fn list_history(
    channel: &Channel,
    synchronizer_id: &str,
    namespace: &str,
    exclude: &[&str],
) -> Result<Vec<HistoryItem>> {
    // `ListAll` is the variant Canton 3.5 serves; its successor is 3.6-only.
    #[allow(deprecated)]
    let response = TopologyManagerReadServiceClient::new(channel.clone())
        .list_all(tonic::Request::new(ListAllRequest {
            base_query: Some(topology::history_query(synchronizer_id)),
            exclude_mappings: exclude.iter().map(|code| (*code).to_string()).collect(),
            filter_namespace: namespace.to_string(),
        }))
        .await?
        .into_inner();
    response
        .result
        .map(|result| result.items)
        .unwrap_or_default()
        .into_iter()
        // A rejected transaction never took effect, so it is not history.
        .filter(|item| item.rejection_reason.is_none())
        .map(|item| {
            Ok(HistoryItem {
                valid_from: item.valid_from,
                signed: utils::decode_versioned(&item.transaction)?,
            })
        })
        .collect()
}

/// The party's DNS and P2P transactions from `items`, and every namespace
/// that signed for them: the owners of every DNS version and the hosting
/// participants of every P2P version.
///
/// A namespace can hold several parties and a filtered read matches on a
/// prefix, so every mapping is pinned to this party exactly.
fn party_replays(
    party_id: &CantonId,
    items: Vec<HistoryItem>,
) -> Result<(Vec<Replay>, BTreeSet<String>)> {
    let namespace = party_id.namespace.to_hex();
    let party = party_id.to_string();
    let mut replays = Vec::new();
    let mut namespaces = BTreeSet::new();
    for item in items {
        let transaction: TopologyTransaction =
            topology_version::decode_transaction(&item.signed.transaction)?;
        let rank = match transaction
            .mapping
            .as_ref()
            .and_then(|m| m.mapping.as_ref())
        {
            Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(def))
                if def.decentralized_namespace == namespace =>
            {
                namespaces.extend(def.owners.iter().cloned());
                Rank::Namespace
            }
            Some(topology_mapping::Mapping::PartyToParticipant(p2p)) if p2p.party == party => {
                namespaces.extend(
                    p2p.participants
                        .iter()
                        .filter_map(|host| participant_namespace(&host.participant_uid)),
                );
                Rank::PartyMapping
            }
            _ => continue,
        };
        replays.push(Replay::new(item, rank, transaction.serial));
    }
    Ok((replays, namespaces))
}

/// The `NamespaceDelegation`s for exactly `namespace` from `items`.
fn delegation_replays(namespace: &str, items: Vec<HistoryItem>) -> Result<Vec<Replay>> {
    let mut replays = Vec::new();
    for item in items {
        let transaction: TopologyTransaction =
            topology_version::decode_transaction(&item.signed.transaction)?;
        let Some(topology_mapping::Mapping::NamespaceDelegation(delegation)) = transaction
            .mapping
            .as_ref()
            .and_then(|m| m.mapping.as_ref())
        else {
            continue;
        };
        if delegation.namespace != namespace {
            continue;
        }
        let root = delegation
            .target_key
            .as_ref()
            .is_some_and(|key| utils::compute_fingerprint(key) == namespace);
        let rank = if root {
            Rank::RootDelegation
        } else {
            Rank::Delegation
        };
        replays.push(Replay::new(item, rank, transaction.serial));
    }
    Ok(replays)
}

impl Replay {
    fn new(item: HistoryItem, rank: Rank, serial: u32) -> Self {
        Self {
            valid_from: item.valid_from.map(|t| (t.seconds, t.nanos)),
            rank,
            serial,
            signed: item.signed,
        }
    }
}

/// The order to replay in: by effective time, then [`Rank`], then serial.
fn replay_order(mut replays: Vec<Replay>) -> Vec<SignedTopologyTransaction> {
    replays.sort_by_key(|replay| (replay.valid_from, replay.rank, replay.serial));
    replays.into_iter().map(|replay| replay.signed).collect()
}

/// The namespace of a participant uid `name::namespace`.
fn participant_namespace(participant_uid: &str) -> Option<String> {
    participant_uid
        .rsplit_once("::")
        .map(|(_, namespace)| namespace.to_string())
        .filter(|namespace| !namespace.is_empty())
}

/// Refuse to sign unless the replay rebuilt the heads the proposals were
/// built on.
///
/// # Errors
///
/// Errors when either head in the store is missing or at another serial.
pub(super) async fn ensure_seeded_to(
    config: &NodeConfig,
    store: &str,
    party_id: &CantonId,
    head: &PartyHead,
) -> Result {
    let dns = topology::fetch_namespace_definition_at_head_in(
        config,
        temporary_store_id(store),
        &party_id.namespace.to_hex(),
    )
    .await
    .map(|(serial, _)| serial);
    let p2p = topology::fetch_p2p_mapping_at_head_in(config, temporary_store_id(store), party_id)
        .await
        .map(|(serial, _)| serial);
    // A read that could not reach the participant says nothing about the
    // store, so it goes back as it is rather than as a failed replay.
    let unreachable = |read: Result<u32>| match read {
        Err(e) if is_unreachable(&e) => {
            Err(e.context(format!("Could not read the temporary store {store}")))
        }
        other => Ok(other),
    };
    let dns = unreachable(dns)?;
    let p2p = unreachable(p2p)?;
    match (dns, p2p) {
        (Ok(dns), Ok(p2p)) if dns == head.dns_serial && p2p == head.p2p_serial => Ok(()),
        (dns, p2p) => anyhow::bail!(
            "Could not rebuild {party_id}'s topology in a temporary store: it holds DNS serial \
             {dns} and P2P serial {p2p}, the synchronizer DNS serial {want_dns} and P2P serial \
             {want_p2p}. Nothing was signed or published",
            dns = describe(&dns),
            p2p = describe(&p2p),
            want_dns = head.dns_serial,
            want_p2p = head.p2p_serial,
        ),
    }
}

fn describe(serial: &Result<u32>) -> String {
    match serial {
        Ok(serial) => serial.to_string(),
        Err(e) => format!("none ({e:#})"),
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningPublicKey,
        protocol::v30::{
            DecentralizedNamespaceDefinition, NamespaceDelegation, PartyToParticipant,
            TopologyMapping, enums, party_to_participant::HostingParticipant,
        },
        version::v1::{UntypedVersionedMessage, untyped_versioned_message},
    };
    use prost::Message;

    use super::*;

    // -- replay ------------------------------------------------------------

    fn party_ns() -> String {
        format!("1220{digest}", digest = "aa".repeat(32))
    }

    fn party() -> Result<CantonId> {
        CantonId::parse(&format!("party::{ns}", ns = party_ns()))
    }

    fn item(seconds: i64, serial: u32, mapping: topology_mapping::Mapping) -> HistoryItem {
        let transaction = TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        };
        HistoryItem {
            valid_from: Some(prost_types::Timestamp { seconds, nanos: 0 }),
            signed: SignedTopologyTransaction {
                transaction: UntypedVersionedMessage {
                    version: 30,
                    wrapper: Some(untyped_versioned_message::Wrapper::Data(
                        transaction.encode_to_vec(),
                    )),
                }
                .encode_to_vec(),
                ..Default::default()
            },
        }
    }

    fn dns(owners: &[&str]) -> topology_mapping::Mapping {
        topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            DecentralizedNamespaceDefinition {
                decentralized_namespace: party_ns(),
                threshold: 1,
                owners: owners.iter().map(|o| (*o).to_string()).collect(),
            },
        )
    }

    fn p2p(party: &str, hosts: &[&str]) -> topology_mapping::Mapping {
        topology_mapping::Mapping::PartyToParticipant(PartyToParticipant {
            party: party.into(),
            threshold: 1,
            participants: hosts
                .iter()
                .map(|uid| HostingParticipant {
                    participant_uid: (*uid).to_string(),
                    permission: enums::ParticipantPermission::Confirmation as i32,
                    onboarding: None,
                })
                .collect(),
            party_signing_keys: None,
        })
    }

    fn serials_and_kinds(order: &[SignedTopologyTransaction]) -> Result<Vec<(u32, &'static str)>> {
        order
            .iter()
            .map(|signed| {
                let tx: TopologyTransaction =
                    topology_version::decode_transaction(&signed.transaction)?;
                let kind = match tx.mapping.and_then(|m| m.mapping) {
                    Some(topology_mapping::Mapping::NamespaceDelegation(_)) => "nsd",
                    Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(_)) => "dnd",
                    Some(topology_mapping::Mapping::PartyToParticipant(_)) => "ptp",
                    _ => "other",
                };
                Ok((tx.serial, kind))
            })
            .collect()
    }

    /// Only this party's mappings are replayed, and the namespaces they name
    /// are what the delegation reads go and fetch.
    #[test]
    fn the_replay_keeps_this_party_and_names_every_signer() -> Result {
        let party = party()?;
        let items = vec![
            item(10, 1, dns(&["owner-a", "owner-b"])),
            item(11, 1, p2p(&party.to_string(), &["p1::ns-p1", "p2::ns-p2"])),
            // Another party in the same namespace, and a prefix lookalike.
            item(
                12,
                1,
                p2p(&format!("other::{ns}", ns = party_ns()), &["p9::ns-p9"]),
            ),
            item(13, 2, dns(&["owner-a", "owner-c"])),
            item(14, 2, p2p(&party.to_string(), &["p1::ns-p1", "p3::ns-p3"])),
        ];

        let (replays, namespaces) = party_replays(&party, items)?;

        assert_eq!(replays.len(), 4);
        assert_eq!(
            namespaces.into_iter().collect::<Vec<_>>(),
            ["ns-p1", "ns-p2", "ns-p3", "owner-a", "owner-b", "owner-c"]
        );
        Ok(())
    }

    /// Each transaction is judged against the state it was signed for, so the
    /// replay follows effective time, and within one moment a key's
    /// delegation precedes what it signed.
    #[test]
    fn the_replay_follows_history_and_puts_delegations_first() -> Result {
        let party = party()?;
        let root_key = SigningPublicKey {
            public_key: vec![9; 32],
            ..Default::default()
        };
        let owner = utils::compute_fingerprint(&root_key).to_string();
        let root = topology_mapping::Mapping::NamespaceDelegation(NamespaceDelegation {
            namespace: owner.clone(),
            target_key: Some(root_key),
            ..Default::default()
        });
        let intermediate = topology_mapping::Mapping::NamespaceDelegation(NamespaceDelegation {
            namespace: owner.clone(),
            target_key: Some(SigningPublicKey {
                public_key: vec![8; 32],
                ..Default::default()
            }),
            ..Default::default()
        });

        let (mut replays, _) = party_replays(
            &party,
            vec![
                item(20, 2, p2p(&party.to_string(), &["p1::ns"])),
                item(10, 1, p2p(&party.to_string(), &["p1::ns"])),
                item(10, 1, dns(&[&owner])),
            ],
        )?;
        replays.extend(delegation_replays(
            &owner,
            vec![item(10, 1, intermediate), item(10, 5, root.clone())],
        )?);
        // A delegation for a namespace that merely starts the same way.
        replays.extend(delegation_replays(
            &owner[..owner.len() - 2],
            vec![item(1, 1, root)],
        )?);

        assert_eq!(
            serials_and_kinds(&replay_order(replays))?,
            [(5, "nsd"), (1, "nsd"), (1, "dnd"), (1, "ptp"), (2, "ptp")]
        );
        Ok(())
    }

    #[test]
    fn a_participant_uid_yields_its_namespace() {
        assert_eq!(
            participant_namespace("participant1::1220ab").as_deref(),
            Some("1220ab")
        );
        assert_eq!(participant_namespace("no-namespace"), None);
        assert_eq!(participant_namespace("trailing::"), None);
    }
}
