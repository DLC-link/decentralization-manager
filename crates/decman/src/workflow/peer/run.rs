use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::{NodeConfig, Peer},
    consts::{MAX_CONSECUTIVE_NO_WORKFLOW_POLLS, peer_wait_poll_delay_ms},
    error::Result,
    noise::{MessageType, NoiseError, client::NoiseClient},
    server::DeclineInvitationPayload,
    utils,
    workflow::{
        storage::{WorkflowStorage, artifact_kinds},
        validation::PeerExpectations,
    },
};

use crate::workflow::{
    add_party, change_threshold, contracts, kick, onboarding, party_replication, rotate_key,
    topology,
};

use super::{
    failure::{record_signature_upload, report_departure, retry_peer_step},
    payload::{
        decode_add_party_config, extract_party_id_from_p2p_payload, is_new_member,
        save_prepared_submissions_from_payload, send_skip_status,
    },
    steps::{command_matches_kind, persist_peer_step},
};

/// Start node in peer mode (client)
/// Called when this node receives a workflow invite from the coordinator.
///
/// `instance_name` is the local peer-side `workflow_runs` row's primary
/// key — accept_invitation creates the row with a synthetic name (e.g.
/// `peer-onboarding-<pubkey>-<ts>`) and we use that same name for every
/// `workflow_artifacts` write, so the FK constraint to `workflow_runs` is
/// satisfied. The coordinator's logical instance_name (carried in
/// OnboardingConfig/ContractsConfig/KickConfig payloads) is the coordinator's
/// own primary key on its DB and is not used for storage on the peer side.
pub async fn start_peer(
    node_config: NodeConfig,
    coordinator: Peer,
    db: SqlitePool,
    instance_name: String,
    coordinator_instance: String,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    tracing::info!(
        "Initializing Noise client to connect to coordinator {}...",
        coordinator.participant_id
    );

    // The invitation the operator accepted, read back before the first
    // command arrives. Every payload this loop is asked to act on is checked
    // against it, so a coordinator cannot widen the run beyond what was
    // agreed. Without it the peer cannot tell what it consented to, so the
    // run fails rather than signing blind.
    let coordinator_id = coordinator.participant_id.clone();
    let expectations =
        PeerExpectations::load(&db, &instance_name, &node_config, &coordinator_id).await?;

    let client = NoiseClient::new(
        node_config.clone(),
        coordinator,
        coordinator_instance.clone(),
    )
    .await?;

    tracing::info!("Noise client initialized, entering command polling loop");

    let kind = expectations.kind;
    let reporter = client.clone();
    let result = run_peer(
        node_config,
        client,
        db,
        instance_name,
        expectations,
        workflow_auth,
    )
    .await;
    if let Err(e) = &result {
        let report = DeclineInvitationPayload {
            kind,
            reason: Some(format!("{e:#}")),
            workflow_instance: (!coordinator_instance.is_empty()).then_some(coordinator_instance),
            abandoned: true,
        };
        tokio::spawn(report_departure(reporter, report));
    }
    result
}

/// Poll the coordinator and execute its commands until it disconnects this
/// peer, or until a failure ends the run.
async fn run_peer(
    node_config: NodeConfig,
    client: NoiseClient,
    db: SqlitePool,
    instance_name: String,
    expectations: PeerExpectations,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    let peer_kind = expectations.kind;

    // Command polling loop
    let mut consecutive_errors = 0;
    let mut consecutive_no_workflow = 0;
    let mut consecutive_step_failures = 0;
    let mut consecutive_upload_failures = 0;
    loop {
        // Poll coordinator for next command (with payload for commands that need data)
        let message = match client.get_next_command_with_payload().await {
            Ok(msg) => {
                consecutive_errors = 0; // Reset error count on success
                consecutive_no_workflow = 0;
                msg
            }
            Err(e) => {
                // A 503 means the coordinator is reachable but has no active
                // workflow registered — either it is still resuming after a
                // restart (transient) or its workflow was cancelled / dismissed
                // while we were offline (permanent). The TCP round-trip
                // succeeded, so reset the connection-error count and instead
                // give the coordinator a bounded number of polls to
                // (re-)register before giving up, so a resumed run doesn't poll
                // a dead coordinator forever. The counter resets on any real
                // reply above, so a slow resume rides through.
                if matches!(&e, NoiseError::BadStatusCode(code, _) if code.as_u16() == 503) {
                    consecutive_errors = 0;
                    consecutive_no_workflow += 1;
                    if consecutive_no_workflow >= MAX_CONSECUTIVE_NO_WORKFLOW_POLLS {
                        tracing::error!(
                            "Coordinator reported no active workflow for \
                             {MAX_CONSECUTIVE_NO_WORKFLOW_POLLS} polls; the workflow was \
                             cancelled or dismissed. Aborting."
                        );
                        anyhow::bail!(
                            "Peer failed: coordinator has no active workflow (cancelled or dismissed)"
                        );
                    }
                    tracing::warn!(
                        "Poll {consecutive_no_workflow}/{MAX_CONSECUTIVE_NO_WORKFLOW_POLLS}: \
                         coordinator has no active workflow yet, retrying"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }

                // The coordinator serves the same payload on every poll, so
                // one above this node's limit never fits. A retry only delays
                // the report.
                if matches!(e, NoiseError::PayloadTooLarge { .. }) {
                    anyhow::bail!("Peer failed on the coordinator's payload: {e}");
                }

                consecutive_errors += 1;

                // Per-attempt failures are WARN, not ERROR — the loop's design
                // is to tolerate transient blips (e.g. coordinator restarting
                // briefly during peer-config reload). Only the final-strike
                // bail is logged at ERROR. This keeps test logs readable when
                // a known restart cycle produces one or two retries that
                // succeed on the next attempt.
                tracing::warn!("Attempt {consecutive_errors}/3: failed to get next command: {e}");

                // If we get multiple connection refused errors in a row,
                // the coordinator has likely shut down or there's a persistent error
                if consecutive_errors >= 3 {
                    tracing::error!(
                        "Failed to communicate with coordinator after 3 attempts. Aborting."
                    );
                    anyhow::bail!(
                        "Peer failed: persistent communication errors with coordinator: {e}"
                    );
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let command = message.msg_type;
        let payload = message.payload;

        // Keep every failure path on the same counter and backoff policy.
        macro_rules! fail_step {
            ($($reason:tt)*) => {
                retry_peer_step(&mut consecutive_step_failures, command, format!($($reason)*)).await?
            };
        }

        // Only commands that belong to the accepted workflow kind are
        // executed. A Contracts invitation must not become a licence to sign
        // a kick's topology transactions.
        if !command_matches_kind(peer_kind, command) {
            fail_step!(
                "Refusing {command:?}: the accepted invitation is for a {peer_kind:?} workflow"
            );
            continue;
        }

        persist_peer_step(&db, &instance_name, peer_kind, command).await;

        match command {
            MessageType::Wait => {
                tracing::trace!("Received command: Wait");
                // Coordinator says to wait, poll again after delay
                tokio::time::sleep(tokio::time::Duration::from_millis(peer_wait_poll_delay_ms()))
                    .await;
            }
            MessageType::RetireLegacyKeys => {
                // The target comes from the accepted invitation, never this command's payload.
                let Some(party) = expectations.dec_party_id.as_ref() else {
                    fail_step!("No accepted party for legacy key retirement");
                    continue;
                };
                // A failure retries like any other step. Ending the run here
                // would skip RecordKey, and this node would keep the old key
                // on record for the coordinator.
                let retired = async {
                    let synchronizer_id = utils::get_synchronizer_id(&node_config).await?;
                    topology::retire_legacy_keys(&node_config, &synchronizer_id, party).await
                }
                .await;
                if let Err(e) = retired {
                    fail_step!("Failed to retire the legacy key mapping: {e:#}");
                    continue;
                }
                consecutive_step_failures = 0;
                client
                    .send(
                        MessageType::StatusUpdate,
                        topology::RETIRED_LEGACY_KEYS_STATUS.to_vec(),
                    )
                    .await?;
            }
            MessageType::Disconnect => {
                tracing::info!("Received disconnect command, shutting down");
                break;
            }
            MessageType::UploadDars => {
                tracing::info!("Executing: Upload DARs");

                // Decode DAR files from payload and upload directly
                let dar_files = if payload.is_empty() {
                    Vec::new()
                } else {
                    match utils::decode_files(&payload) {
                        Ok(files) => files,
                        Err(e) => {
                            fail_step!("Failed to decode DARs from coordinator: {e}");
                            continue;
                        }
                    }
                };

                // Vetting a DAR makes this participant willing to execute its
                // code, so the files must be the ones the operator accepted.
                if let Err(e) = expectations.check_dars(&dar_files) {
                    fail_step!("Refusing the coordinator's DARs: {e}");
                    continue;
                }

                if let Err(e) = contracts::upload_dars_from_bytes(&node_config, dar_files).await {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = client
                    .send(MessageType::StatusUpdate, b"UploadDars completed".to_vec())
                    .await
                {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::GenerateKeys => {
                tracing::info!("Executing: Generate keys");
                // Deserialize onboarding config from payload (we still need
                // the prefix for namespace_key_name / daml_key_name; the
                // config's `instance_name` is the coordinator's view and is
                // intentionally unused here).
                let onboarding_config: onboarding::OnboardingConfig =
                    match serde_json::from_slice(&payload) {
                        Ok(config) => config,
                        Err(e) => {
                            fail_step!("Failed to deserialize onboarding config: {e}");
                            continue;
                        }
                    };

                // The prefix names the vault keys this step creates or reuses,
                // so it is pinned before any key material is touched.
                if let Err(e) =
                    expectations.check_onboarding_config(&onboarding_config.party_id_prefix)
                {
                    fail_step!("Refusing the coordinator's onboarding config: {e}");
                    continue;
                }

                if let Err(e) =
                    onboarding::generate_keys(&node_config, &db, &instance_name, &onboarding_config)
                        .await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = onboarding::peer::send_keys_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send keys to coordinator: {e}");
                }
            }
            MessageType::SignDns => {
                tracing::info!("Executing: Sign DNS proposal");
                if payload.is_empty() {
                    fail_step!("No DNS proposal payload received from coordinator");
                    continue;
                }
                match expectations
                    .check_onboarding_dns(&node_config, &db, &instance_name, &payload)
                    .await
                {
                    // Remember the namespace we authorized so the P2P
                    // proposal that follows can be pinned to the same party.
                    Ok(namespace) => {
                        // The P2P proposal is pinned to this namespace in the
                        // next step. Signing DNS without recording it would
                        // leave that cross-check silently disabled, so a write
                        // failure fails the step instead.
                        if let Err(e) = db
                            .write_artifact(
                                &instance_name,
                                artifact_kinds::ACCEPTED_DNS_NAMESPACE,
                                None,
                                namespace.as_bytes(),
                            )
                            .await
                        {
                            fail_step!(
                                "Refusing to sign DNS: the accepted namespace could not be \
                                 recorded, so the P2P cross-check would be lost: {e}"
                            );
                            continue;
                        }
                    }
                    Err(e) => {
                        fail_step!("Refusing the coordinator's DNS proposal: {e}");
                        continue;
                    }
                }
                if let Err(e) =
                    onboarding::sign_dns_proposals(&node_config, &db, &instance_name, &payload)
                        .await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                record_signature_upload(
                    &mut consecutive_upload_failures,
                    command,
                    onboarding::peer::send_dns_signature_to_coordinator(
                        &client,
                        &db,
                        &instance_name,
                        &node_config,
                    )
                    .await,
                )
                .await?;
            }
            MessageType::SignP2p => {
                tracing::info!("Executing: Sign P2P proposals");
                if payload.is_empty() {
                    fail_step!("No P2P proposal payload received from coordinator");
                    continue;
                }
                // Absent means the DNS step ran on a build that did not record
                // the namespace yet — the mixed-version case, which degrades to
                // the prefix check with a warning inside check_onboarding_p2p.
                // A read error is different: it hides whether a namespace was
                // recorded at all, so it fails the step rather than skipping
                // the cross-check.
                let signed_namespace = match db
                    .read_artifact(&instance_name, artifact_kinds::ACCEPTED_DNS_NAMESPACE, None)
                    .await
                {
                    // Nothing recorded is the mixed-version case. Bytes that
                    // are not valid UTF-8 are not: something WAS recorded and
                    // cannot be read back, so treating it as absent would drop
                    // the cross-check on corrupt state.
                    Ok(None) => None,
                    Ok(Some(bytes)) => match String::from_utf8(bytes) {
                        Ok(namespace) => Some(namespace),
                        Err(e) => {
                            fail_step!(
                                "Refusing to sign P2P: the recorded DNS namespace is not \
                                 readable, so the cross-check cannot be applied: {e}"
                            );
                            continue;
                        }
                    },
                    Err(e) => {
                        fail_step!(
                            "Refusing to sign P2P: cannot read the DNS namespace this run \
                             accepted, so the cross-check cannot be applied: {e}"
                        );
                        continue;
                    }
                };
                if let Err(e) = expectations
                    .check_onboarding_p2p(
                        &node_config,
                        &db,
                        &instance_name,
                        &payload,
                        signed_namespace.as_deref(),
                    )
                    .await
                {
                    fail_step!("Refusing the coordinator's P2P proposal: {e}");
                    continue;
                }
                if let Err(e) =
                    onboarding::file_key_and_sign_p2p(&node_config, &db, &instance_name, &payload)
                        .await
                {
                    fail_step!("{e:#}");
                    continue;
                }
                consecutive_step_failures = 0;
                record_signature_upload(
                    &mut consecutive_upload_failures,
                    command,
                    onboarding::peer::send_p2p_signatures_to_coordinator(
                        &client,
                        &db,
                        &instance_name,
                        &node_config,
                    )
                    .await,
                )
                .await?;

                // Identity hook (peer side): the SignP2p payload is a
                // SignedTopologyTransaction whose PartyToParticipant mapping
                // carries the resolved dec_party_id in its `party` field. By
                // now the namespace has been signed and submitted by the
                // coordinator, so we can persist this peer's keys +
                // participant id under the dec_party_identity table for use
                // by post-onboarding workflows on this node.
                match extract_party_id_from_p2p_payload(&payload) {
                    Ok(dec_party_id) => {
                        if let Err(e) = db.write_run_party_id(&instance_name, &dec_party_id).await {
                            tracing::error!(
                                "Failed to persist party id {dec_party_id} on peer run: {e}"
                            );
                        }
                        if let Err(e) = onboarding::peer::copy_self_identity_for_party(
                            &db,
                            &instance_name,
                            &node_config,
                            &dec_party_id,
                        )
                        .await
                        {
                            tracing::error!("Failed to copy self identity for {dec_party_id}: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to extract dec_party_id from P2P payload: {e}");
                    }
                }
            }
            MessageType::SignSubmissions => {
                tracing::info!("Executing: Sign submissions");

                if payload.is_empty() {
                    fail_step!("No submissions payload received from coordinator");
                    continue;
                }

                // Decode config and files from payload: [config_json, files_payload]
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        fail_step!("Failed to decode SignSubmissions payload: {e}");
                        continue;
                    }
                };

                let contracts_config: contracts::ContractsConfig =
                    match serde_json::from_slice(&items[0]) {
                        Ok(config) => config,
                        Err(e) => {
                            fail_step!("Failed to deserialize contracts config: {e}");
                            continue;
                        }
                    };

                let dec_party_id = contracts_config.decentralized_party_id.clone();
                if let Err(e) = expectations.check_dec_party(&dec_party_id) {
                    fail_step!("Refusing the coordinator's contracts config: {e}");
                    continue;
                }

                // Persist the prepared submissions sent by the coordinator into
                // this peer's workflow_artifacts so sign_submissions can
                // read them back via list_artifacts.
                if let Err(e) =
                    save_prepared_submissions_from_payload(&items[1], &db, &instance_name).await
                {
                    fail_step!("Failed to save prepared submissions from coordinator: {e}");
                    continue;
                }

                if let Err(e) = contracts::sign_submissions(
                    &node_config,
                    &db,
                    &instance_name,
                    &dec_party_id,
                    &expectations.contract_intents,
                )
                .await
                {
                    fail_step!("Step execution failed: {e:#}");
                    continue;
                }
                consecutive_step_failures = 0;
                record_signature_upload(
                    &mut consecutive_upload_failures,
                    command,
                    contracts::peer::send_submission_signatures_to_coordinator(
                        &client,
                        &db,
                        &instance_name,
                        &node_config,
                    )
                    .await,
                )
                .await?;
            }
            MessageType::SignKick => {
                tracing::info!("Executing: Sign kick proposals");
                if payload.is_empty() {
                    fail_step!("No kick proposals payload received from coordinator");
                    continue;
                }

                let items = match utils::decode_length_prefixed(&payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        fail_step!("Failed to decode SignKick payload: {e}");
                        continue;
                    }
                };

                let _kick_config: kick::KickConfig = match serde_json::from_slice(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        fail_step!("Failed to deserialize kick config: {e}");
                        continue;
                    }
                };

                if let Err(e) = expectations
                    .check_party_proposals(&node_config, &db, &instance_name, &items[1], &items[2])
                    .await
                {
                    fail_step!("Refusing the coordinator's kick proposals: {e}");
                    continue;
                }

                let kick_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) =
                    kick::sign_proposals(&node_config, &db, &instance_name, &kick_data).await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                record_signature_upload(
                    &mut consecutive_upload_failures,
                    command,
                    kick::peer::send_kick_signatures_to_coordinator(
                        &client,
                        &db,
                        &instance_name,
                        &node_config,
                    )
                    .await,
                )
                .await?;
            }
            MessageType::SignChangeThreshold => {
                tracing::info!("Executing: Sign change-threshold proposals");
                if payload.is_empty() {
                    fail_step!("No change-threshold proposals payload received from coordinator");
                    continue;
                }

                let items = match utils::decode_length_prefixed(&payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        fail_step!("Failed to decode SignChangeThreshold payload: {e}");
                        continue;
                    }
                };

                let _change_config: change_threshold::ChangeThresholdConfig =
                    match serde_json::from_slice(&items[0]) {
                        Ok(config) => config,
                        Err(e) => {
                            fail_step!("Failed to deserialize change-threshold config: {e}");
                            continue;
                        }
                    };

                if let Err(e) = expectations
                    .check_party_proposals(&node_config, &db, &instance_name, &items[1], &items[2])
                    .await
                {
                    fail_step!("Refusing the coordinator's change-threshold proposals: {e}");
                    continue;
                }

                let proposal_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) = change_threshold::sign_proposals(
                    &node_config,
                    &db,
                    &instance_name,
                    &proposal_data,
                )
                .await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                record_signature_upload(
                    &mut consecutive_upload_failures,
                    command,
                    change_threshold::peer::send_change_threshold_signatures_to_coordinator(
                        &client,
                        &db,
                        &instance_name,
                        &node_config,
                    )
                    .await,
                )
                .await?;
            }
            MessageType::SignRotateKey => {
                tracing::info!("Executing: Sign rotate-key proposal");
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        fail_step!("Failed to decode SignRotateKey payload: {e}");
                        continue;
                    }
                };
                if let Err(e) = serde_json::from_slice::<rotate_key::RotateKeyConfig>(&items[0]) {
                    fail_step!("Failed to deserialize rotate-key config: {e}");
                    continue;
                }
                let new_key = match expectations
                    .check_rotation_proposal(&node_config, &db, &instance_name, &items[1])
                    .await
                {
                    Ok(new_key) => new_key,
                    Err(e) => {
                        fail_step!("Refusing the coordinator's rotate-key proposal: {e}");
                        continue;
                    }
                };
                // RecordKey later records only the key checked here.
                if let Err(e) = db
                    .write_artifact(
                        &instance_name,
                        artifact_kinds::ROTATE_KEY_ACCEPTED_NEW_KEY,
                        None,
                        new_key.as_str().as_bytes(),
                    )
                    .await
                {
                    fail_step!("Failed to record the checked rotation key: {e}");
                    continue;
                }
                if let Err(e) =
                    rotate_key::steps::sign_proposal(&node_config, &db, &instance_name, &items[1])
                        .await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = rotate_key::peer::send_rotate_key_signatures_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send rotate-key signatures to coordinator: {e}");
                }
            }
            MessageType::RecordRotatedKey => {
                tracing::info!("Executing: Record the coordinator's new Daml key");
                // The party, the coordinator and the replaced key all come
                // from the accepted invitation. The payload names the new
                // key, which must be the one this node checked in
                // SignProposal, and which the topology must already carry.
                let accepted = db
                    .read_artifact(
                        &instance_name,
                        artifact_kinds::ROTATE_KEY_ACCEPTED_NEW_KEY,
                        None,
                    )
                    .await;
                let recorded = match (
                    expectations.dec_party_id.as_ref(),
                    expectations.rotated_key.as_ref(),
                    accepted,
                ) {
                    (_, _, Err(e)) => Err(e),
                    (_, _, Ok(None)) => Err(anyhow::anyhow!(
                        "this node checked no proposal of this run, so it records no key"
                    )),
                    (Some(party), Some(rotated_key), Ok(Some(accepted))) => {
                        match (
                            rotate_key::steps::decode_recorded_key(&payload),
                            String::from_utf8(accepted)
                                .map_err(anyhow::Error::from)
                                .and_then(|key| key.parse::<crate::canton_id::Fingerprint>()),
                        ) {
                            (Ok(new_key), Ok(accepted)) => {
                                rotate_key::steps::record_peer_key(
                                    &node_config,
                                    &db,
                                    party,
                                    &expectations.coordinator_id,
                                    (rotated_key, &accepted),
                                    new_key,
                                )
                                .await
                            }
                            (Err(e), _) | (_, Err(e)) => Err(e),
                        }
                    }
                    (_, _, Ok(Some(_))) => Err(anyhow::anyhow!(
                        "the accepted rotate-key invitation names no party or no replaced key"
                    )),
                };
                if let Err(e) = recorded {
                    fail_step!("Failed to record the coordinator's new key: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                client
                    .send(
                        MessageType::StatusUpdate,
                        rotate_key::RECORDED_KEY_STATUS.to_vec(),
                    )
                    .await?;
            }
            MessageType::GenerateAddPartyKeys => {
                tracing::info!("Executing: Generate add-party keys");
                let add_party_config = match decode_add_party_config(&payload) {
                    Ok(config) => config,
                    Err(e) => {
                        fail_step!("Failed to deserialize add-party config: {e:#}");
                        continue;
                    }
                };
                // Both the party and the new member are pinned before
                // `is_new_member` reads `new_participant_id` to decide this
                // node's role: naming this peer there would otherwise make it
                // take the new member's part in a run it joined as an existing
                // member.
                if let Err(e) = expectations
                    .check_dec_party(&add_party_config.decentralized_party_id)
                    .and_then(|()| {
                        expectations.check_new_participant(&add_party_config.new_participant_id)
                    })
                {
                    fail_step!("Refusing the coordinator's add-party config: {e}");
                    continue;
                }
                if !is_new_member(&node_config, &add_party_config) {
                    consecutive_step_failures = 0;
                    send_skip_status(&client, "GenerateAddPartyKeys").await;
                    continue;
                }

                let ledger_token = add_party::resolve_ledger_token(
                    &workflow_auth,
                    &add_party_config.decentralized_party_id,
                )
                .await;
                if let Err(e) = add_party::generate_keys(
                    &node_config,
                    &db,
                    &instance_name,
                    &add_party_config,
                    ledger_token.as_deref(),
                )
                .await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                if let Err(e) = add_party::peer::send_keys_to_coordinator(
                    &client,
                    &db,
                    &instance_name,
                    &node_config,
                )
                .await
                {
                    tracing::error!("Failed to send add-party keys to coordinator: {e}");
                }
            }
            MessageType::SignAddParty => {
                tracing::info!("Executing: Sign add-party proposals");
                if payload.is_empty() {
                    fail_step!("No add-party proposals payload received from coordinator");
                    continue;
                }

                let items = match utils::decode_length_prefixed(&payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        fail_step!("Failed to decode SignAddParty payload: {e}");
                        continue;
                    }
                };
                let add_party_config = match decode_add_party_config(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        fail_step!("Failed to deserialize add-party config: {e:#}");
                        continue;
                    }
                };

                let add_party_check = async {
                    expectations.check_dec_party(&add_party_config.decentralized_party_id)?;
                    expectations.check_new_participant(&add_party_config.new_participant_id)?;
                    expectations
                        .check_party_proposals(
                            &node_config,
                            &db,
                            &instance_name,
                            &items[1],
                            &items[2],
                        )
                        .await
                }
                .await;
                if let Err(e) = add_party_check {
                    fail_step!("Refusing the coordinator's add-party proposals: {e}");
                    continue;
                }

                if is_new_member(&node_config, &add_party_config)
                    && let Err(e) =
                        party_replication::check_vetted_packages_held(&node_config).await
                {
                    fail_step!("Refusing to sign as the new member: {e:#}");
                    continue;
                }

                let proposal_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) =
                    add_party::sign_proposals(&node_config, &db, &instance_name, &proposal_data)
                        .await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                // The new member leaves the synchronizer before its signature can
                // let the coordinator authorize the mapping. From the moment the
                // mapping is effective it receives the party's traffic, and it
                // must not journal any of it until the ACS import has landed.
                // Its own store has to hold the proposal first: disconnected, it
                // will not see the mapping take effect, and the import checks for it.
                if is_new_member(&node_config, &add_party_config)
                    && let Err(e) = async {
                        add_party::peer::wait_for_hosting_proposal(
                            &node_config,
                            &add_party_config.decentralized_party_id,
                        )
                        .await?;
                        party_replication::open_import_window(
                            &node_config,
                            &db,
                            &add_party_config.replication_target(&instance_name),
                        )
                        .await
                    }
                    .await
                {
                    fail_step!("Failed to open the ACS import window: {e:#}");
                    continue;
                }
                consecutive_step_failures = 0;
                record_signature_upload(
                    &mut consecutive_upload_failures,
                    command,
                    add_party::peer::send_add_party_signatures_to_coordinator(
                        &client,
                        &db,
                        &instance_name,
                        &node_config,
                    )
                    .await,
                )
                .await?;

                // Identity hook (new member): same point in the protocol
                // where onboarding peers persist their identity — the party
                // id is authoritative from the config, and the keys were
                // written by GenerateAddPartyKeys.
                if is_new_member(&node_config, &add_party_config)
                    && let Err(e) = onboarding::peer::copy_self_identity_for_party(
                        &db,
                        &instance_name,
                        &node_config,
                        &add_party_config.decentralized_party_id,
                    )
                    .await
                {
                    tracing::error!(
                        "Failed to copy self identity for {party}: {e}",
                        party = add_party_config.decentralized_party_id
                    );
                }
            }
            MessageType::ImportAcs => {
                tracing::info!("Executing: Import party ACS");
                // [config, package_ids]. The snapshot is not in the payload: it
                // is pulled block by block straight into Canton's import, so
                // nothing proportional to the ACS is buffered or stored.
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        fail_step!("Failed to decode ImportAcs payload: {e}");
                        continue;
                    }
                };
                let add_party_config = match decode_add_party_config(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        fail_step!("Failed to deserialize add-party config: {e:#}");
                        continue;
                    }
                };
                // Both the party and the new member are pinned before
                // `is_new_member` reads `new_participant_id` to decide this
                // node's role: naming this peer there would otherwise make it
                // take the new member's part in a run it joined as an existing
                // member.
                if let Err(e) = expectations
                    .check_dec_party(&add_party_config.decentralized_party_id)
                    .and_then(|()| {
                        expectations.check_new_participant(&add_party_config.new_participant_id)
                    })
                {
                    fail_step!("Refusing the coordinator's ACS import: {e}");
                    continue;
                }
                if !is_new_member(&node_config, &add_party_config) {
                    consecutive_step_failures = 0;
                    send_skip_status(&client, "ImportAcs").await;
                    continue;
                }
                // Newline-joined package ids the party's contracts need; the
                // import preflight checks this participant has them all before
                // it disconnects anything.
                let required_package_ids: Vec<String> = String::from_utf8_lossy(&items[1])
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();

                let step_result = party_replication::import_party_acs(
                    &node_config,
                    &db,
                    &add_party_config.replication_target(&instance_name),
                    &required_package_ids,
                    party_replication::TransferSource::Coordinator,
                    |seq| {
                        let client = &client;
                        async move { client.request_next_acs_block(seq).await.map_err(Into::into) }
                    },
                )
                .await;

                // The import owns its retries and runs them inside one disconnect
                // window. Retrying the step here would reconnect the participant
                // between attempts, which replays the ACS journal — so a failure
                // that reaches this point ends the peer.
                if let Err(e) = step_result {
                    anyhow::bail!("Aborting peer: the ACS import failed: {e:#}");
                }
                consecutive_step_failures = 0;
                if let Err(e) = client
                    .send(MessageType::StatusUpdate, b"ImportAcs completed".to_vec())
                    .await
                {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::ClearOnboardingFlag => {
                tracing::info!("Executing: Clear onboarding flag");
                let add_party_config = match decode_add_party_config(&payload) {
                    Ok(config) => config,
                    Err(e) => {
                        fail_step!("Failed to deserialize add-party config: {e:#}");
                        continue;
                    }
                };
                // Both the party and the new member are pinned before
                // `is_new_member` reads `new_participant_id` to decide this
                // node's role: naming this peer there would otherwise make it
                // take the new member's part in a run it joined as an existing
                // member.
                if let Err(e) = expectations
                    .check_dec_party(&add_party_config.decentralized_party_id)
                    .and_then(|()| {
                        expectations.check_new_participant(&add_party_config.new_participant_id)
                    })
                {
                    fail_step!("Refusing the coordinator's clearing request: {e}");
                    continue;
                }
                if !is_new_member(&node_config, &add_party_config) {
                    consecutive_step_failures = 0;
                    send_skip_status(&client, "ClearOnboardingFlag").await;
                    continue;
                }

                match party_replication::clear_onboarding_flag(
                    &node_config,
                    &db,
                    &add_party_config.replication_target(&instance_name),
                )
                .await
                {
                    Ok(party_replication::ClearOutcome::Proposed) => {
                        // Canton requires the ONBOARDING PARTICIPANT to issue
                        // the flag-clear transaction — author it here and ship
                        // it to the coordinator for the threshold-signing
                        // round (the coordinator lacks an appropriate key).
                        match add_party::author_clear_proposal(&node_config, &add_party_config)
                            .await
                        {
                            Ok(Some(proposal)) => {
                                consecutive_step_failures = 0;
                                record_signature_upload(
                                    &mut consecutive_upload_failures,
                                    command,
                                    client
                                        .send(MessageType::AddPartyClearProposal, proposal)
                                        .await
                                        .map_err(Into::into),
                                )
                                .await?;
                            }
                            Ok(None) => {
                                // Flag dropped between the poll and the
                                // authoring — report as cleared.
                                consecutive_step_failures = 0;
                                if let Err(e) = client
                                    .send(MessageType::AddPartyClearProposal, Vec::new())
                                    .await
                                {
                                    tracing::error!("Failed to send cleared marker: {e}");
                                }
                            }
                            Err(e) => {
                                fail_step!("Step execution failed: {e}");
                            }
                        }
                    }
                    Ok(party_replication::ClearOutcome::Cleared) => {
                        consecutive_step_failures = 0;
                        if let Err(e) = client
                            .send(MessageType::AddPartyClearProposal, Vec::new())
                            .await
                        {
                            tracing::error!("Failed to send cleared marker: {e}");
                        }
                    }
                    Err(e) => {
                        fail_step!("Step execution failed: {e}");
                    }
                }
            }
            MessageType::SignClearOnboarding => {
                tracing::info!("Executing: Sign onboarding-flag clearing proposal");
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        fail_step!("Failed to decode SignClearOnboarding payload: {e}");
                        continue;
                    }
                };
                if items[1].is_empty() {
                    // Skip marker: the flag already cleared without a
                    // signing round (e.g. a single-owner-threshold party).
                    consecutive_step_failures = 0;
                    send_skip_status(&client, "SignClearOnboarding").await;
                    continue;
                }

                if let Err(e) = expectations.check_clear_onboarding(&items[1]) {
                    fail_step!("Refusing the coordinator's clearing proposal: {e}");
                    continue;
                }

                if let Err(e) =
                    add_party::sign_clear_proposal(&node_config, &db, &instance_name, &items[1])
                        .await
                {
                    fail_step!("Step execution failed: {e}");
                    continue;
                }
                consecutive_step_failures = 0;
                record_signature_upload(
                    &mut consecutive_upload_failures,
                    command,
                    add_party::peer::send_clear_signature_to_coordinator(
                        &client,
                        &db,
                        &instance_name,
                        &node_config,
                    )
                    .await,
                )
                .await?;
            }
            _ => {
                tracing::warn!("Unexpected message type: {command:?}");
            }
        }
    }

    tracing::info!("Peer shutting down");
    Ok(())
}
