use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::enums::ParticipantPermission,
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use crate::{config::NodeConfig, error::Result, utils, workflow::topology};

/// A head-state (or proposal) topology query against the synchronizer store.
pub(super) fn party_query(synchronizer_id: &str, proposals: bool) -> BaseQuery {
    BaseQuery {
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
            })),
        }),
        proposals,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    }
}

/// Whether this participant hosts an external party yet, from its own topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostOnboardingStatus {
    /// An authorized `PartyToParticipant` names this participant with
    /// Confirmation and no onboarding marker — hosting is live.
    Hosted,
    /// Named in the authorized mapping, but Canton's onboarding marker is still
    /// set: the party is assigned here and suspended here. It holds none of the
    /// party's contracts and cannot confirm for it until the marker clears.
    ///
    /// Distinct from [`Pending`](Self::Pending), which is about signatures the
    /// topology still needs. This one is authorized and waiting on replication.
    Onboarding,
    /// A proposal exists but is not yet fully authorized (waiting on more hosts).
    Pending,
    /// No mapping — authorized or proposed — for this party on this participant.
    Absent,
}

/// Read whether this participant hosts `party_id` from its own topology state via
/// the tokenless Canton Admin API. There is no local run row: each host answers
/// only for itself, and the wallet aggregates across hosts.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or the
/// `ListPartyToParticipant` RPC fails.
pub async fn host_onboarding_status(
    config: &NodeConfig,
    party_id: &str,
) -> Result<HostOnboardingStatus> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let self_uid = config.participant_id().to_string();
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    // Authorized head state first (proposals = false); if absent, a pending
    // proposal (proposals = true) means this host has not finished signing.
    for proposals in [false, true] {
        let response = client
            .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
                base_query: Some(party_query(&synchronizer_id, proposals)),
                filter_party: party_id.to_string(),
                filter_participant: self_uid.clone(),
            }))
            .await?
            .into_inner();
        // The marker matters as much as the name: a host carrying it is in the
        // mapping but suspended, holding none of the party's contracts. Reading
        // only the uid and the permission reports such a host as live, which is
        // how a caller ends up trusting a host that cannot yet confirm anything.
        let mut named = None;
        for (_, p) in topology::p2p_rows(response.results)? {
            if let Some(entry) = p.participants.iter().find(|h| {
                h.participant_uid == self_uid
                    && h.permission == ParticipantPermission::Confirmation as i32
            }) {
                named = Some(entry.onboarding.is_some());
                break;
            }
        }
        if let Some(marked) = named {
            return Ok(if proposals {
                HostOnboardingStatus::Pending
            } else if marked {
                HostOnboardingStatus::Onboarding
            } else {
                HostOnboardingStatus::Hosted
            });
        }
    }
    Ok(HostOnboardingStatus::Absent)
}

/// One external party this participant hosts, read from topology.
#[derive(Clone)]
pub struct HostedExternalParty {
    pub party_id: String,
    pub fingerprint: String,
    /// How many of the hosting participants must confirm a transaction (the M).
    pub threshold: u32,
    /// How many participants host the party (the N).
    pub host_count: u32,
    /// When the mapping became effective, RFC 3339. `None` if Canton did not
    /// report it.
    pub created_at: Option<String>,
    /// Whether this node still carries Canton's onboarding marker for the party.
    ///
    /// `true` means the party is assigned here and suspended here: it holds none
    /// of the party's contracts and confirms nothing. Listing such a party
    /// without saying so is how an operator concludes a replication finished
    /// when it has not.
    pub onboarding: bool,
    /// The participants the mapping names, in the order it lists them.
    pub hosts: Vec<HostedExternalPartyHost>,
}

/// One participant named by a hosted external party's mapping. `permission` is
/// the raw Canton `ParticipantPermission` discriminant: mapping it to the wire
/// enum is the server's job (see `server::types::permission_from_proto`), which
/// keeps the proto dependency out of the wire DTOs.
#[derive(Clone)]
pub struct HostedExternalPartyHost {
    pub participant_uid: String,
    pub permission: i32,
}

/// Render a protobuf timestamp as RFC 3339 (UTC), for display.
fn timestamp_to_rfc3339(ts: &prost_types::Timestamp) -> String {
    chrono::DateTime::from_timestamp(ts.seconds, ts.nanos.max(0) as u32)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// List the external parties this participant hosts with Confirmation permission,
/// from its own topology (tokenless Admin API). External = a self-owned
/// single-key namespace: excludes decentralized parties (their namespace is a
/// `DecentralizedNamespaceDefinition`) and this node's own local namespace.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or a topology RPC
/// fails.
pub async fn list_hosted_external_parties(config: &NodeConfig) -> Result<Vec<HostedExternalParty>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let self_uid = config.participant_id().to_string();
    let own_namespace = self_uid
        .rsplit_once("::")
        .map(|(_, ns)| ns.to_string())
        .unwrap_or_default();
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let decentralized: std::collections::HashSet<String> = client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(party_query(&synchronizer_id, false)),
                filter_namespace: String::new(),
            },
        ))
        .await?
        .into_inner()
        .results
        .into_iter()
        .filter_map(|r| r.item.map(|i| i.decentralized_namespace))
        .collect();

    let p2p = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(party_query(&synchronizer_id, false)),
            filter_party: String::new(),
            filter_participant: self_uid.clone(),
        }))
        .await?
        .into_inner();

    let mut parties = Vec::new();
    for (context, mapping) in topology::p2p_rows(p2p.results)? {
        // `valid_from` is when this mapping became effective — the party's creation
        // time for a serial-1 mapping.
        let created_at = context
            .as_ref()
            .and_then(|c| c.valid_from.as_ref())
            .map(timestamp_to_rfc3339);
        let Some(entry) = mapping.participants.iter().find(|h| {
            h.participant_uid == self_uid
                && h.permission == ParticipantPermission::Confirmation as i32
        }) else {
            continue;
        };
        let onboarding = entry.onboarding.is_some();
        let Some((_, fingerprint)) = mapping.party.rsplit_once("::") else {
            continue;
        };
        let fingerprint = fingerprint.to_string();
        // Skip this node's own local namespace and any decentralized party — what
        // remains is an external party with a self-owned single-key namespace.
        if fingerprint == own_namespace || decentralized.contains(&fingerprint) {
            continue;
        }
        let hosts = mapping
            .participants
            .iter()
            .map(|h| HostedExternalPartyHost {
                participant_uid: h.participant_uid.clone(),
                permission: h.permission,
            })
            .collect();
        parties.push(HostedExternalParty {
            party_id: mapping.party.clone(),
            fingerprint,
            threshold: mapping.threshold,
            host_count: mapping.participants.len() as u32,
            created_at,
            onboarding,
            hosts,
        });
    }
    Ok(parties)
}
