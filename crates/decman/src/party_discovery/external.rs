//! The external parties this participant hosts, read from its own topology.

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::enums::ParticipantPermission,
    topology::admin::v30::{
        ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use crate::{config::NodeConfig, error::Result, utils, workflow::topology};

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
                base_query: Some(topology::head_state_query(&synchronizer_id)),
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
            base_query: Some(topology::head_state_query(&synchronizer_id)),
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
