use std::time::Duration;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        PartyToParticipant, enums::ParticipantPermission, party_to_participant::HostingParticipant,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ForceFlag, ListDecentralizedNamespaceDefinitionRequest,
        authorize_request, topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use super::{
    attempt::{Finished, Stop, read_hosted},
    error::UnhostError,
};
use crate::{
    config::NodeConfig,
    consts::topology_retry_delay_secs,
    utils,
    workflow::{topology, topology_version::ProposalFormat},
};

/// How many head-state reads the topology step makes, one topology retry delay
/// apart, before it reports the write as accepted but not yet effective. Kept
/// short so the whole call returns inside a wallet client's 60 s timeout.
const EFFECTIVE_POLLS: u32 = 10;

/// The namespace half of a party id, if it has one.
fn namespace_of(party_id: &str) -> Option<&str> {
    party_id.rsplit_once("::").map(|(_, namespace)| namespace)
}

/// Refuse a decentralized party.
///
/// Its hosts change only through its governance: a host leaves it by a kick
/// its members vote for. Without this check, anyone holding a tenant key could
/// make this node leave a governance party on this node's key alone, because
/// Canton accepts a single host's departure from any party.
///
/// # Errors
/// [`UnhostError::Refused`] for a decentralized party, and
/// [`UnhostError::Internal`] when the topology read fails.
pub(super) async fn refuse_a_decentralized_party(
    config: &NodeConfig,
    party_id: &str,
) -> Result<(), UnhostError> {
    let Some(namespace) = namespace_of(party_id) else {
        return Ok(());
    };
    let decentralized = async {
        let synchronizer_id = utils::get_synchronizer_id(config).await?;
        let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
        let response = client
            .list_decentralized_namespace_definition(tonic::Request::new(
                ListDecentralizedNamespaceDefinitionRequest {
                    base_query: Some(topology::head_state_query(&synchronizer_id)),
                    filter_namespace: namespace.to_string(),
                },
            ))
            .await
            .context("ListDecentralizedNamespaceDefinition RPC failed")?
            .into_inner();
        anyhow::Ok(response.results.iter().any(|result| {
            result
                .item
                .as_ref()
                .is_some_and(|definition| definition.decentralized_namespace == namespace)
        }))
    }
    .await
    .map_err(UnhostError::Internal)?;
    if decentralized {
        return Err(UnhostError::Refused(format!(
            "{party_id} is a decentralized party; its hosts change only through its governance, \
             so remove a member with a kick instead"
        )));
    }
    Ok(())
}

/// Does the mapping list `me`, at any permission?
pub(super) fn hosts(mapping: &PartyToParticipant, me: &str) -> bool {
    mapping.participants.iter().any(|p| p.participant_uid == me)
}

/// Can this host act for the party? Unmarked, and at Confirmation or
/// Submission. The same rule add-hosts applies: a marked host holds none of the
/// party's contracts, and an Observation host confirms nothing.
fn can_act(host: &HostingParticipant) -> bool {
    host.onboarding.is_none()
        && (host.permission == ParticipantPermission::Confirmation as i32
            || host.permission == ParticipantPermission::Submission as i32)
}

/// The serial-N+1 mapping without `me`: same party, same threshold, same keys,
/// and every other host in its current order.
///
/// # Errors
/// [`UnhostError::Refused`] when `me` is the last host, or when the hosts left
/// could not meet the threshold.
pub(super) fn leave_mapping(
    current: &PartyToParticipant,
    me: &str,
) -> Result<PartyToParticipant, UnhostError> {
    let party = &current.party;
    let remaining: Vec<HostingParticipant> = current
        .participants
        .iter()
        .filter(|p| p.participant_uid != me)
        .cloned()
        .collect();
    if remaining.len() == current.participants.len() {
        return Err(UnhostError::NotHosted {
            party: party.clone(),
        });
    }
    if remaining.is_empty() {
        return Err(UnhostError::Refused(format!(
            "this host is the last host of {party}; Canton refuses a mapping with no hosts"
        )));
    }
    let able = remaining.iter().filter(|p| can_act(p)).count();
    if (able as u32) < current.threshold {
        return Err(UnhostError::Refused(format!(
            "{party} needs {threshold} confirmations, and only {able} of the hosts that would \
             remain can give one; the party's key must lower the threshold first \
             (/v0/tenant/threshold/prepare, then /v0/tenant/threshold/execute)",
            threshold = current.threshold
        )));
    }
    Ok(PartyToParticipant {
        party: party.clone(),
        threshold: current.threshold,
        participants: remaining,
        party_signing_keys: current.party_signing_keys.clone(),
    })
}

/// Write the mapping without this node and wait, briefly, for it to take
/// effect.
///
/// Head state is read first, so a repeat after the write landed reads as
/// success, and a party that moved since the preconditions is a stale pin.
/// The serial at submission is `base_serial + 1`, so of two concurrent writers
/// only one can succeed.
pub(super) async fn leave_topology(
    config: &NodeConfig,
    party_id: &str,
    me: &str,
    base_serial: u32,
) -> Result<Finished, Stop> {
    let current = read_hosted(config, party_id).await.map_err(Stop::Failed)?;
    if !hosts(&current.mapping, me) {
        return Ok(Finished::Completed {
            serial: current.serial,
        });
    }
    if current.serial != base_serial {
        return Err(UnhostError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        }
        .into());
    }
    let mapping = leave_mapping(&current.mapping, me)?;
    let submitted = base_serial.checked_add(1).ok_or_else(|| {
        UnhostError::Refused(format!(
            "{party_id} is at serial {base_serial}, which cannot be advanced"
        ))
    })?;
    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(Stop::Failed)?;

    let authorized = topology::authorize_with_topology_retry(
        config,
        AuthorizeRequest {
            r#type: Some(authorize_request::Type::Proposal(
                ProposalFormat::for_synchronizer(&synchronizer_id).proposal(
                    submitted,
                    topology_mapping::Mapping::PartyToParticipant(mapping),
                ),
            )),
            // This node's namespace alone authorizes its own departure, so the
            // write must be complete when this returns.
            must_fully_authorize: true,
            // Canton refuses to drop a party this node still holds active
            // contracts for unless told the operator means it. Keeping those
            // contracts in the store is deliberate: purging them is separate
            // work, because other local parties may still see them.
            force_changes: vec![ForceFlag::DisablePartyWithActiveContracts as i32],
            signed_by: vec![],
            store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            wait_to_become_effective: None,
        },
        "external-party unhost authorize",
    )
    .await;

    if let Err(error) = authorized {
        // The write may have landed before the error, or another writer may
        // have taken the serial. Head state says which.
        return match read_hosted(config, party_id).await {
            Ok(after) if !hosts(&after.mapping, me) => Ok(Finished::Completed {
                serial: after.serial,
            }),
            Ok(after) if after.serial != base_serial => Err(UnhostError::StaleSerial {
                party: party_id.to_string(),
                pinned: base_serial,
                found: after.serial,
            }
            .into()),
            // Unchanged head state does not prove a rejection when the answer
            // was lost: Canton may have scheduled the removal anyway. The
            // watcher keeps the party held until it knows.
            _ if outcome_unknown(&error) => {
                tracing::warn!(
                    %party_id,
                    "external-party unhost: the topology write's outcome is unknown: {error:#}"
                );
                Ok(Finished::Pending { submitted })
            }
            _ => Err(Stop::Failed(error)),
        };
    }

    let delay = Duration::from_secs(topology_retry_delay_secs());
    for _ in 0..EFFECTIVE_POLLS {
        match read_hosted(config, party_id).await {
            Ok(after) if !hosts(&after.mapping, me) => {
                return Ok(Finished::Completed {
                    serial: after.serial,
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(%party_id, "external-party unhost: head-state read failed: {e:#}")
            }
        }
        tokio::time::sleep(delay).await;
    }
    Ok(Finished::Pending { submitted })
}

/// Whether a failed topology write may still have been scheduled.
///
/// A gRPC error that says the call never finished cannot tell a rejection from
/// a lost answer. An error that is not a gRPC status failed before anything
/// was sent, and a status such as `INVALID_ARGUMENT` is Canton's refusal.
fn outcome_unknown(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<tonic::Status>())
        .is_some_and(|status| {
            matches!(
                status.code(),
                tonic::Code::Unavailable
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::Cancelled
                    | tonic::Code::Unknown
                    | tonic::Code::Internal
                    | tonic::Code::Aborted
            )
        })
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::hosting_participant;

    use super::*;
    use crate::workflow::external_party::unhost::test_support::*;

    fn host(uid: &str, permission: ParticipantPermission, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: uid.to_string(),
            permission: permission as i32,
            onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
        }
    }

    /// A 2-of-3 party hosted by P1, ME and P2, in that order.
    fn two_of_three() -> PartyToParticipant {
        PartyToParticipant {
            party: PARTY.to_string(),
            threshold: 2,
            participants: vec![
                host(
                    "participant-1::1220aa",
                    ParticipantPermission::Confirmation,
                    false,
                ),
                host(ME, ParticipantPermission::Confirmation, false),
                host(
                    "participant-2::1220bb",
                    ParticipantPermission::Confirmation,
                    false,
                ),
            ],
            party_signing_keys: None,
        }
    }

    fn refusal(result: Result<PartyToParticipant, UnhostError>) -> String {
        match result {
            Err(UnhostError::Refused(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The write Canton authorizes on this node's key alone: the same
    /// threshold, no host added, exactly one removed. Anything more needs the
    /// party's key.
    #[test]
    fn the_new_mapping_drops_exactly_this_host() {
        let current = two_of_three();
        let Ok(next) = leave_mapping(&current, ME) else {
            panic!("a 2-of-3 party can lose one host");
        };
        let uids: Vec<&str> = next
            .participants
            .iter()
            .map(|p| p.participant_uid.as_str())
            .collect();
        assert_eq!(uids, vec!["participant-1::1220aa", "participant-2::1220bb"]);
        assert_eq!(next.threshold, current.threshold);
        assert_eq!(next.party, current.party);
        assert_eq!(next.participants[0], current.participants[0]);
        assert_eq!(next.participants[1], current.participants[2]);
        assert_eq!(next.party_signing_keys, current.party_signing_keys);
    }

    #[test]
    fn the_last_host_cannot_leave() {
        let current = PartyToParticipant {
            threshold: 1,
            participants: vec![host(ME, ParticipantPermission::Confirmation, false)],
            ..two_of_three()
        };
        assert!(refusal(leave_mapping(&current, ME)).contains("last host"));
    }

    /// On 2 of 2 neither host may leave: the one left could not meet the
    /// threshold. The message names the remedy.
    #[test]
    fn no_host_may_leave_a_party_at_full_threshold() {
        let mut current = two_of_three();
        current.participants.remove(2);
        let message = refusal(leave_mapping(&current, ME));
        assert!(message.contains("threshold/prepare"), "{message}");
        assert!(message.contains("only 1"), "{message}");
    }

    /// A host still carrying the onboarding marker holds none of the party's
    /// contracts and confirms nothing, so it does not count as a host that
    /// remains.
    #[test]
    fn a_marked_host_does_not_count_as_remaining() {
        let mut current = two_of_three();
        current.participants[2].onboarding = Some(hosting_participant::Onboarding {});
        refusal(leave_mapping(&current, ME));
    }

    #[test]
    fn an_observation_host_does_not_count_as_remaining() {
        let mut current = two_of_three();
        current.participants[2].permission = ParticipantPermission::Observation as i32;
        refusal(leave_mapping(&current, ME));
    }

    /// A Submission host exists only at threshold 1 and acts alone, so it is
    /// enough to stay behind.
    #[test]
    fn a_submission_host_counts_as_remaining() {
        let current = PartyToParticipant {
            threshold: 1,
            participants: vec![
                host(
                    "participant-1::1220aa",
                    ParticipantPermission::Submission,
                    false,
                ),
                host(ME, ParticipantPermission::Submission, false),
            ],
            ..two_of_three()
        };
        assert!(leave_mapping(&current, ME).is_ok());
    }

    #[test]
    fn a_mapping_without_this_host_is_not_hosted() {
        let mut current = two_of_three();
        current.participants.remove(1);
        assert!(matches!(
            leave_mapping(&current, ME),
            Err(UnhostError::NotHosted { .. })
        ));
        assert!(!hosts(&current, ME));
    }

    /// A retry after success must read as success, so the check for "this node
    /// is gone" must not care what permission it had.
    #[test]
    fn this_host_is_present_at_any_permission() {
        let mut current = two_of_three();
        current.participants[1].permission = ParticipantPermission::Observation as i32;
        assert!(hosts(&current, ME));
    }

    /// A lost answer must not read as a rejection: the removal may be scheduled,
    /// and releasing the hold would let a relay start under it. Canton's own
    /// refusals, and failures before anything was sent, are definite.
    #[test]
    fn only_an_unfinished_call_leaves_the_outcome_unknown() {
        let status = |code| anyhow::Error::new(tonic::Status::new(code, "x"));
        for code in [
            tonic::Code::Unavailable,
            tonic::Code::DeadlineExceeded,
            tonic::Code::Cancelled,
            tonic::Code::Unknown,
        ] {
            assert!(outcome_unknown(&status(code)), "{code:?}");
        }
        for code in [
            tonic::Code::InvalidArgument,
            tonic::Code::FailedPrecondition,
            tonic::Code::PermissionDenied,
            tonic::Code::AlreadyExists,
        ] {
            assert!(!outcome_unknown(&status(code)), "{code:?}");
        }
        let wrapped = status(tonic::Code::Unavailable).context("authorize");
        assert!(
            outcome_unknown(&wrapped),
            "context must not hide the status"
        );
        assert!(!outcome_unknown(&anyhow::anyhow!(
            "no channel to the admin API"
        )));
    }

    #[test]
    fn a_party_namespace_is_the_part_after_the_separator() {
        assert_eq!(namespace_of("alice::1220aa"), Some("1220aa"));
        assert_eq!(namespace_of("alice"), None);
    }
}
