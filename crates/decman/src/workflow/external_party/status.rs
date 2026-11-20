use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::enums::ParticipantPermission,
    topology::admin::v30::{
        BaseQuery, ListPartyToParticipantRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use crate::{config::NodeConfig, error::Result, utils, workflow::topology};

pub use crate::party_discovery::{
    HostedExternalParty, HostedExternalPartyHost, list_hosted_external_parties,
};

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
                base_query: Some(BaseQuery {
                    proposals,
                    ..topology::head_state_query(&synchronizer_id)
                }),
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
