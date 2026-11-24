use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::PartyToParticipant,
    topology::admin::v30::{
        ListPartyToParticipantRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use crate::{config::NodeConfig, error::Result, utils, workflow::topology};

/// The party's current authorized `PartyToParticipant`, with the serial it sits
/// at. The serial is what the next write increments, and what the wallet pins so
/// every host builds the same bytes.
#[derive(Clone, Debug)]
pub struct CurrentPartyTopology {
    /// Serial of the authorized mapping in head state.
    pub serial: u32,
    /// The mapping itself.
    pub mapping: PartyToParticipant,
}

/// Read `party_id`'s authorized `PartyToParticipant` from this node's head state.
///
/// Returns `None` when no authorized mapping exists — the party is unknown to
/// this node, or exists only as an unsigned proposal.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved, the
/// `ListPartyToParticipant` RPC fails, or Canton reports a negative serial.
pub async fn read_party_to_participant(
    config: &NodeConfig,
    party_id: &str,
) -> Result<Option<CurrentPartyTopology>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let response = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(topology::head_state_query(&synchronizer_id)),
            filter_party: party_id.to_string(),
            filter_participant: String::new(),
        }))
        .await
        .context("ListPartyToParticipant RPC failed")?
        .into_inner();

    for (context, mapping) in topology::p2p_rows(response.results)? {
        let serial = context.as_ref().map(|c| c.serial).unwrap_or_default();
        // `filter_party` is a prefix filter on some Canton versions, so confirm the
        // exact party rather than trusting the server-side filter.
        if mapping.party != party_id {
            continue;
        }
        let serial = u32::try_from(serial).with_context(|| {
            format!("Canton reported serial {serial} for {party_id}, which is not a valid serial")
        })?;
        return Ok(Some(CurrentPartyTopology { serial, mapping }));
    }
    Ok(None)
}
