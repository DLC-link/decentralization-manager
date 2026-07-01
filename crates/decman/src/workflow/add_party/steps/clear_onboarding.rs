use std::time::SystemTime;

use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        ClearPartyOnboardingFlagRequest,
        party_management_service_client::PartyManagementServiceClient,
    },
    protocol::v30::{PartyToParticipant, SignedTopologyTransaction, topology_mapping},
    topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        add_party::{
            AddPartyConfig,
            steps::{
                export_state::{encode_length_prefixed_message, fetch_p2p_mapping},
                proposals::create::proposal_request,
                proposals::submit::add_transactions_request,
            },
        },
        storage::{WorkflowStorage, artifact_kinds},
        topology::{authorize_with_topology_retry, sign_transactions_with_topology_retry},
    },
};

/// Hard cap on how long the new member waits for Canton's "earliest safe
/// time to clear the onboarding flag" to arrive. The safe time is normally
/// seconds away (decision timeouts); ten minutes flags a genuinely stuck
/// synchronizer instead of hanging the workflow forever.
const MAX_SAFE_TIME_WAIT_SECS: u64 = 600;

/// Outcome of the new member's `ClearPartyOnboardingFlag` polling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearOutcome {
    /// Canton reports the flag is gone — no signing round needed.
    Cleared,
    /// The safe time passed and the clearing transaction is proposed; for a
    /// decentralized party it now needs threshold owner signatures, which the
    /// coordinator's sign round provides.
    Proposed,
}

/// New-member step: drive `ClearPartyOnboardingFlag` until the flag is gone
/// or Canton has accepted the clearing proposal past its safe time.
///
/// Canton refuses to clear before its computed safe time (so no in-flight
/// transaction from the import window can be lost) — the endpoint returns
/// `onboarded = false` with `earliest_retry_timestamp` until then. After the
/// safe time, a call proposes the clearing topology transaction; for a
/// decentralized party that proposal still needs threshold owner signatures,
/// which the workflow's next steps collect.
pub async fn clear_onboarding_flag(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result<ClearOutcome> {
    // Logical synchronizer id — see `current_ledger_offset` for why the
    // physical id is rejected by PartyManagementService.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;
    let self_id = config.participant_id().to_string();

    // No zero fallback: ClearPartyOnboardingFlag rejects non-positive
    // offsets, and a missing artifact means GenerateNewMemberKeys never
    // persisted one — a real bug to surface, not paper over.
    let offset_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_PRE_ACTIVATION_OFFSET,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "ADD_PARTY_PRE_ACTIVATION_OFFSET artifact missing for {self_id} — \
                 did GenerateNewMemberKeys run?"
            )
        })?;
    let begin_offset_exclusive: i64 = String::from_utf8(offset_bytes)?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse pre-activation offset: {e}"))?;

    let mut client = PartyManagementServiceClient::connect(config.admin_api_url()).await?;
    let waited_start = time::Instant::now();

    loop {
        let request = tonic::Request::new(ClearPartyOnboardingFlagRequest {
            party_id: add_party_config.decentralized_party_id.to_string(),
            synchronizer_id: synchronizer_id.clone(),
            begin_offset_exclusive,
            wait_for_activation_timeout: None,
        });

        let response = client
            .clear_party_onboarding_flag(request)
            .await?
            .into_inner();

        if response.onboarded {
            tracing::info!("Onboarding flag already cleared");
            return Ok(ClearOutcome::Cleared);
        }

        let now = SystemTime::now();
        let earliest_retry = response
            .earliest_retry_timestamp
            .and_then(|ts| SystemTime::try_from(ts).ok());

        match earliest_retry {
            Some(safe_time) if safe_time > now => {
                let wait = safe_time
                    .duration_since(now)
                    .unwrap_or(time::Duration::from_secs(1))
                    // Re-check at least every 10s so a moving safe time
                    // can't park the loop on one long sleep.
                    .min(time::Duration::from_secs(10));
                if waited_start.elapsed().as_secs() > MAX_SAFE_TIME_WAIT_SECS {
                    anyhow::bail!(
                        "Onboarding-flag safe time still {wait:?} away after waiting \
                         {MAX_SAFE_TIME_WAIT_SECS}s — synchronizer appears stuck"
                    );
                }
                tracing::info!(
                    "Safe time for clearing the onboarding flag not reached; waiting {wait:?}"
                );
                time::sleep(wait).await;
            }
            // Safe time reached (or Canton sent none): this call was made
            // past it, so the clearing transaction is proposed. The
            // coordinator's signing round takes it from here.
            _ => {
                tracing::info!(
                    "Safe time passed; clearing transaction proposed, awaiting owner signatures"
                );
                return Ok(ClearOutcome::Proposed);
            }
        }
    }
}

/// New-member side: author the clearing proposal and return it encoded as
/// the `varint(len)||proto` blob the coordinator persists/ships. `None`
/// when the flag is already gone.
pub async fn author_clear_proposal(
    config: &NodeConfig,
    add_party_config: &AddPartyConfig,
) -> Result<Option<Vec<u8>>> {
    Ok(create_clear_proposal(config, add_party_config)
        .await?
        .map(|transaction| encode_length_prefixed_message(&transaction)))
}

/// Build the onboarding-flag clearing proposal — the current P2P mapping
/// with the new member's `Onboarding` marker removed. MUST run on the new
/// member: Canton requires the onboarding participant itself to issue the
/// flag-clear transaction (the coordinator's authorize fails with
/// TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE — observed live). Returns
/// `None` when the flag is already gone from head state.
pub async fn create_clear_proposal(
    config: &NodeConfig,
    add_party_config: &AddPartyConfig,
) -> Result<Option<SignedTopologyTransaction>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let party_id = &add_party_config.decentralized_party_id;
    let new_member = add_party_config.new_participant_id.to_string();

    let current_p2p = fetch_p2p_mapping(config, &synchronizer_id, party_id).await?;
    if !has_onboarding_marker(&current_p2p, &new_member) {
        tracing::info!("Onboarding flag already cleared in head state — skipping sign round");
        return Ok(None);
    }

    let mut cleared_p2p = current_p2p;
    for participant in &mut cleared_p2p.participants {
        if participant.participant_uid == new_member {
            participant.onboarding = None;
        }
    }

    tracing::info!("Creating onboarding-flag clearing proposal...");
    let response = authorize_with_topology_retry(
        config,
        proposal_request(
            &synchronizer_id,
            topology_mapping::Mapping::PartyToParticipant(cleared_p2p),
        ),
        "add-party-clear",
    )
    .await?;

    response
        .transaction
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("No clearing transaction returned"))
}

/// All-peer step: sign the clearing proposal. `proposal_data` is the single
/// `varint(len)||proto` blob from the coordinator (config stripped by the
/// peer loop). Persists the per-peer `SIGNED_ADD_PARTY_CLEAR` artefact.
pub async fn sign_clear_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    tracing::info!("Signing onboarding-flag clearing proposal...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(proposal_data)?;

    let request = SignTransactionsRequest {
        transactions: vec![transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    };

    let response =
        sign_transactions_with_topology_retry(config, request, "add-party-clear").await?;
    if response.transactions.len() != 1 {
        anyhow::bail!(
            "Expected 1 signed clearing transaction, got {count}",
            count = response.transactions.len()
        );
    }

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
            Some(&node_id),
            &encode_length_prefixed_message(&response.transactions[0]),
        )
        .await?;

    tracing::info!("Clearing proposal signed successfully");
    Ok(())
}

/// Coordinator: aggregate the peers' signatures onto the clearing proposal,
/// submit it, and wait for the onboarding marker to disappear from head
/// state.
pub async fn submit_clear_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let proposal_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("ADD_PARTY_CLEAR_PROPOSAL artifact missing"))?;
    let mut transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&proposal_bytes)?;

    let signed = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_ADD_PARTY_CLEAR)
        .await?;
    tracing::info!(
        "Aggregating clearing-proposal signatures from {count} peer(s)",
        count = signed.len()
    );
    for (peer_id, signed_bytes) in &signed {
        let peer_signed: SignedTopologyTransaction =
            utils::read_first_message_from_bytes(signed_bytes)?;
        tracing::debug!("Adding clearing signatures from {peer_id}");
        transaction.signatures.extend(peer_signed.signatures);
    }

    // The clearing proposal was AUTHORED BY THE NEW MEMBER, so — unlike the
    // add proposals, which the coordinator authors and thus self-signs — it
    // does not yet carry the coordinator's signature. The coordinator is a
    // namespace owner whose signature counts toward the authorization
    // threshold; without it, a party configured with `new_threshold` equal to
    // the owner count could never clear the flag (and every lower threshold
    // loses one otherwise-eligible signer). Add the coordinator's signature.
    let self_signed = sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: vec![transaction.clone()],
            signed_by: vec![],
            store: Some(StoreId {
                store: Some(store_id::Store::Synchronizer(Synchronizer {
                    kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
                })),
            }),
            force_flags: vec![],
        },
        "add-party-clear coordinator",
    )
    .await?;
    if let Some(coordinator_signed) = self_signed.transactions.into_iter().next() {
        transaction.signatures.extend(coordinator_signed.signatures);
    }

    super::proposals::submit::dedupe_signatures(&mut transaction);

    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;
    tracing::info!("Submitting onboarding-flag clearing transaction...");
    topology_write_client
        .add_transactions(tonic::Request::new(add_transactions_request(
            &synchronizer_id,
            transaction,
        )))
        .await?;

    wait_for_flag_cleared(
        config,
        &synchronizer_id,
        &add_party_config.decentralized_party_id,
        &add_party_config.new_participant_id,
    )
    .await
}

/// Poll head state until the new member's onboarding marker is gone.
pub async fn wait_for_flag_cleared(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
    new_member: &CantonId,
) -> Result {
    let max_attempts = crate::consts::topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(crate::consts::topology_retry_delay_secs());
    let new_member_str = new_member.to_string();

    for attempt in 1..=max_attempts {
        let p2p = fetch_p2p_mapping(config, synchronizer_id, party_id).await?;
        if !has_onboarding_marker(&p2p, &new_member_str) {
            tracing::info!("Onboarding flag cleared after {attempt} attempt(s)");
            return Ok(());
        }
        if attempt < max_attempts {
            tracing::debug!(
                "Onboarding flag still set, attempt {attempt}/{max_attempts}, \
                 retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!("Onboarding flag was not cleared after {max_attempts} attempts")
}

/// Whether `participant` carries the Onboarding marker in `p2p`.
fn has_onboarding_marker(p2p: &PartyToParticipant, participant: &str) -> bool {
    p2p.participants
        .iter()
        .any(|p| p.participant_uid == participant && p.onboarding.is_some())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::{
        HostingParticipant, hosting_participant,
    };

    use super::*;

    fn p2p(participants: Vec<HostingParticipant>) -> PartyToParticipant {
        PartyToParticipant {
            party: "acme::1220abcd".to_string(),
            threshold: 2,
            participants,
            party_signing_keys: None,
        }
    }

    fn hosting(uid: &str, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: uid.to_string(),
            permission: 0,
            onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
        }
    }

    #[test]
    fn detects_onboarding_marker_only_for_the_marked_participant() {
        let mapping = p2p(vec![hosting("PAR::a", false), hosting("PAR::b", true)]);

        assert!(!has_onboarding_marker(&mapping, "PAR::a"));
        assert!(has_onboarding_marker(&mapping, "PAR::b"));
        assert!(!has_onboarding_marker(&mapping, "PAR::missing"));
    }
}
