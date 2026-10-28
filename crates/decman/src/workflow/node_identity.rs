//! Canton-native node identity: each node owns the party
//! `decman-node-<participant prefix>::<participant namespace>`.
//!
//! A peer derives that id from the node's participant id and checks it in its
//! own topology store. Only the participant's namespace owner can authorize a
//! party in that namespace, so a mapping found there was made by that node.

use std::time::Duration;

use anyhow::Context;
use canton_proto_rs::com::{
    daml::ledger::api::v2::admin::{
        AllocatePartyRequest, ListUserRightsRequest, Right,
        right::{CanActAs, CanExecuteAs, CanReadAs, Kind},
    },
    digitalasset::canton::{
        protocol::v30::{PartyToParticipant, enums::TopologyChangeOp},
        topology::admin::v30::{
            ListPartyToParticipantRequest,
            topology_manager_read_service_client::TopologyManagerReadServiceClient,
        },
    },
};
use common::types::Permission;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    server::permission_from_proto,
    utils,
    workflow::topology::{head_state_query, p2p_rows},
};

/// The identifier prefix every node party starts with.
pub const NODE_PARTY_PREFIX: &str = "decman-node-";

/// The node party of `participant`.
pub fn node_party_id(participant: &CantonId) -> CantonId {
    CantonId::new(
        format!("{NODE_PARTY_PREFIX}{prefix}", prefix = participant.prefix),
        participant.namespace,
    )
}

/// Whether `party` lives in `participant`'s namespace.
pub fn namespace_matches(party: &CantonId, participant: &CantonId) -> bool {
    party.namespace == participant.namespace
}

/// How one participant hosts one party, read from the synchronizer head state.
#[derive(Clone, Debug)]
pub struct HostingCheck {
    /// Whether an authorized `PartyToParticipant` exists for exactly the party.
    pub mapping_exists: bool,
    /// Whether the participant is among the party's hosts.
    pub hosted: bool,
    /// The participant's permission, when hosted.
    pub permission: Option<Permission>,
}

impl HostingCheck {
    /// Hosted with Submission permission.
    pub fn has_submission(&self) -> bool {
        self.hosted && matches!(self.permission, Some(Permission::Submission))
    }
}

/// Evaluate `mapping` against `participant`.
pub fn hosting_check_from_mapping(
    mapping: Option<&PartyToParticipant>,
    participant: &CantonId,
) -> HostingCheck {
    let participant_uid = participant.to_string();
    let host = mapping.and_then(|m| {
        m.participants
            .iter()
            .find(|h| h.participant_uid == participant_uid)
    });
    HostingCheck {
        mapping_exists: mapping.is_some(),
        hosted: host.is_some(),
        permission: host.map(|h| permission_from_proto(h.permission)),
    }
}

/// The mapping for exactly `party`. Canton's `filter_party` matches by prefix,
/// so it also returns parties such as `<identifier>-decoy` in the same
/// namespace.
pub fn exact_mapping(
    rows: Vec<PartyToParticipant>,
    party: &CantonId,
) -> Option<PartyToParticipant> {
    let party = party.to_string();
    rows.into_iter().find(|m| m.party == party)
}

/// The head-state mappings whose party matches `filter_party` by prefix.
async fn list_party_to_participant(
    config: &NodeConfig,
    filter_party: &str,
) -> Result<Vec<PartyToParticipant>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let response = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(head_state_query(&synchronizer_id)),
            filter_party: filter_party.to_string(),
            filter_participant: String::new(),
        }))
        .await
        .context("ListPartyToParticipant RPC failed")?
        .into_inner();
    // The head state can hold a Remove, which is no mapping at all.
    Ok(p2p_rows(response.results)?
        .into_iter()
        .filter(|(context, _)| {
            context
                .as_ref()
                .is_some_and(|c| c.operation == TopologyChangeOp::AddReplace as i32)
        })
        .map(|(_, mapping)| mapping)
        .collect())
}

/// Read how `participant` hosts exactly `party` from this node's topology.
///
/// # Errors
///
/// Returns an error when the topology read fails.
pub async fn verify_hosting(
    config: &NodeConfig,
    party: &CantonId,
    participant: &CantonId,
) -> Result<HostingCheck> {
    let rows = list_party_to_participant(config, &party.to_string()).await?;
    Ok(hosting_check_from_mapping(
        exact_mapping(rows, party).as_ref(),
        participant,
    ))
}

/// [`verify_hosting`], retried each second for up to `wait` until the
/// participant hosts the party with Submission.
///
/// # Errors
///
/// Returns an error when a topology read fails.
pub async fn wait_for_submission(
    config: &NodeConfig,
    party: &CantonId,
    participant: &CantonId,
    wait: Duration,
) -> Result<HostingCheck> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let check = verify_hosting(config, party, participant).await?;
        if check.has_submission() || tokio::time::Instant::now() >= deadline {
            return Ok(check);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Every party whose identifier starts with [`NODE_PARTY_PREFIX`], with no
/// exact-match filter.
///
/// # Errors
///
/// Returns an error when the topology read fails.
pub async fn scan_node_parties(config: &NodeConfig) -> Result<Vec<String>> {
    let mut parties: Vec<String> = list_party_to_participant(config, NODE_PARTY_PREFIX)
        .await?
        .into_iter()
        .map(|m| m.party)
        .collect();
    parties.sort();
    parties.dedup();
    Ok(parties)
}

/// Allocate the party `hint` on this participant with act-as for `user_id`.
/// Returns the party id Canton allocated, or `None` when the party already
/// exists.
///
/// # Errors
///
/// Returns an error when the AllocateParty call fails for another reason.
pub async fn allocate_node_party(
    config: &NodeConfig,
    admin_token: &str,
    hint: &str,
    user_id: &str,
) -> Result<Option<String>> {
    let mut client = utils::create_party_client(config, Some(admin_token.to_string())).await?;
    let request = AllocatePartyRequest {
        party_id_hint: hint.to_string(),
        local_metadata: None,
        identity_provider_id: String::new(),
        // Empty is accepted while the participant is on one synchronizer.
        synchronizer_id: String::new(),
        user_id: user_id.to_string(),
    };
    match client.allocate_party(tonic::Request::new(request)).await {
        Ok(response) => response
            .into_inner()
            .party_details
            .map(|details| Some(details.party))
            .context("AllocateParty returned no party details"),
        Err(status) if status.code() == tonic::Code::AlreadyExists => Ok(None),
        Err(status) => Err(status).context("AllocateParty RPC failed"),
    }
}

/// The rights `user_id` holds, read with `token`.
///
/// # Errors
///
/// Returns an error when the ListUserRights call fails.
pub async fn list_user_rights(
    config: &NodeConfig,
    token: &str,
    user_id: &str,
) -> Result<Vec<Right>> {
    let mut client = utils::create_user_client(config, Some(token.to_string())).await?;
    Ok(client
        .list_user_rights(tonic::Request::new(ListUserRightsRequest {
            user_id: user_id.to_string(),
            identity_provider_id: String::new(),
        }))
        .await
        .context("ListUserRights RPC failed")?
        .into_inner()
        .rights)
}

/// Whether `rights` let the user act as and read as `party`.
pub fn rights_flags(rights: &[Right], party: &str) -> (bool, bool) {
    let mut act_as = false;
    let mut read_as = false;
    for right in rights {
        match &right.kind {
            Some(Kind::CanActAs(CanActAs { party: p })) if p == party => act_as = true,
            Some(Kind::CanReadAs(CanReadAs { party: p })) if p == party => read_as = true,
            _ => {}
        }
    }
    (act_as, read_as)
}

/// Each right as a short label.
pub fn right_names(rights: &[Right]) -> Vec<String> {
    rights
        .iter()
        .map(|right| match &right.kind {
            Some(Kind::ParticipantAdmin(_)) => "ParticipantAdmin".to_string(),
            Some(Kind::IdentityProviderAdmin(_)) => "IdentityProviderAdmin".to_string(),
            Some(Kind::CanActAs(CanActAs { party })) => format!("CanActAs({party})"),
            Some(Kind::CanReadAs(CanReadAs { party })) => format!("CanReadAs({party})"),
            Some(Kind::CanExecuteAs(CanExecuteAs { party })) => format!("CanExecuteAs({party})"),
            Some(Kind::CanActAsAnyParty(_)) => "CanActAsAnyParty".to_string(),
            Some(Kind::CanReadAsAnyParty(_)) => "CanReadAsAnyParty".to_string(),
            Some(Kind::CanExecuteAsAnyParty(_)) => "CanExecuteAsAnyParty".to_string(),
            None => "Unknown".to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::{
        daml::ledger::api::v2::admin::right::ParticipantAdmin,
        digitalasset::canton::protocol::v30::{
            enums::ParticipantPermission, party_to_participant::HostingParticipant,
        },
    };

    use super::*;

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    const OTHER_NS: &str = "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";

    fn id(prefix: &str, ns: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::{ns}")).expect("valid id")
    }

    fn mapping(
        party: &CantonId,
        hosts: &[(&CantonId, ParticipantPermission)],
    ) -> PartyToParticipant {
        PartyToParticipant {
            party: party.to_string(),
            threshold: 1,
            participants: hosts
                .iter()
                .map(|(p, perm)| HostingParticipant {
                    participant_uid: p.to_string(),
                    permission: *perm as i32,
                    onboarding: None,
                })
                .collect(),
            party_signing_keys: None,
        }
    }

    fn right(kind: Kind) -> Right {
        Right { kind: Some(kind) }
    }

    #[test]
    fn node_party_keeps_the_participant_prefix_and_namespace() {
        let party = node_party_id(&id("participant1", NS));
        assert_eq!(party.to_string(), format!("decman-node-participant1::{NS}"));
    }

    #[test]
    fn namespace_matches_only_the_hosting_participant() {
        let participant = id("participant1", NS);
        let party = node_party_id(&participant);
        assert!(namespace_matches(&party, &participant));
        assert!(!namespace_matches(&party, &id("participant1", OTHER_NS)));
    }

    #[test]
    fn submission_host_passes_the_check() {
        let participant = id("participant1", NS);
        let m = mapping(
            &node_party_id(&participant),
            &[(&participant, ParticipantPermission::Submission)],
        );
        assert!(hosting_check_from_mapping(Some(&m), &participant).has_submission());
    }

    #[test]
    fn confirmation_only_host_fails_the_check() {
        let participant = id("participant1", NS);
        let m = mapping(
            &node_party_id(&participant),
            &[(&participant, ParticipantPermission::Confirmation)],
        );
        let check = hosting_check_from_mapping(Some(&m), &participant);
        assert!(check.hosted);
        assert!(matches!(check.permission, Some(Permission::Confirmation)));
        assert!(!check.has_submission());
    }

    #[test]
    fn a_different_participant_is_not_hosted() {
        let participant = id("participant1", NS);
        let m = mapping(
            &node_party_id(&participant),
            &[(&participant, ParticipantPermission::Submission)],
        );
        let check = hosting_check_from_mapping(Some(&m), &id("participant2", OTHER_NS));
        assert!(check.mapping_exists);
        assert!(!check.hosted);
        assert!(!check.has_submission());
    }

    #[test]
    fn no_mapping_is_not_hosted() {
        let check = hosting_check_from_mapping(None, &id("participant1", NS));
        assert!(!check.mapping_exists);
        assert!(!check.has_submission());
    }

    #[test]
    fn exact_mapping_drops_prefix_colliders() {
        let participant = id("participant", NS);
        let party = node_party_id(&participant);
        let decoy = id("decman-node-participant-decoy", NS);
        let rows = vec![
            mapping(&decoy, &[(&participant, ParticipantPermission::Submission)]),
            mapping(
                &party,
                &[(&participant, ParticipantPermission::Confirmation)],
            ),
        ];
        let found = exact_mapping(rows.clone(), &party).expect("the real party");
        assert_eq!(found.party, party.to_string());
        assert!(exact_mapping(rows[..1].to_vec(), &party).is_none());
    }

    #[test]
    fn rights_flags_read_act_as_and_read_as() {
        let party = "decman-node-participant1::ns";
        let rights = [
            right(Kind::CanActAs(CanActAs {
                party: party.to_string(),
            })),
            right(Kind::CanReadAs(CanReadAs {
                party: "other::ns".to_string(),
            })),
        ];
        assert_eq!(rights_flags(&rights, party), (true, false));
        assert_eq!(rights_flags(&rights, "other::ns"), (false, true));
    }

    #[test]
    fn right_names_include_participant_admin() {
        let rights = [
            right(Kind::ParticipantAdmin(ParticipantAdmin {})),
            right(Kind::CanActAs(CanActAs {
                party: "p::ns".to_string(),
            })),
        ];
        assert_eq!(
            right_names(&rights),
            ["ParticipantAdmin", "CanActAs(p::ns)"]
        );
    }
}
