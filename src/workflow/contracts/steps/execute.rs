use bytes::Buf;
use sqlx::SqlitePool;
use uuid::Uuid;

use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CumulativeFilter, EventFormat, Filters, GetEventsByContractIdRequest, Signature,
        Transaction, WildcardFilter, cumulative_filter, event,
        interactive::{
            ExecuteSubmissionAndWaitForTransactionRequest, PartySignatures,
            PrepareSubmissionResponse, SinglePartySignatures,
        },
    },
    digitalasset::canton::crypto::v30::Signature as CantonSignature,
};

use crate::{
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    utils,
    workflow::{
        contracts::ContractsConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Execute signed ledger submissions
///
/// This step must be run by the coordinator with appropriate Ledger API credentials.
/// It aggregates all signatures and executes the prepared submissions on the ledger.
///
/// # Arguments
/// * `config` - Configuration with Ledger API connection details
/// * `db` - Workflow storage backend
/// * `instance_name` - Workflow run instance name (key for `workflow_artifacts`)
/// * `contracts_config` - Contracts workflow configuration with party ID
/// * `token` - Authentication token for Ledger API
/// * `user_id` - User ID for Ledger API operations
pub async fn execute_submissions(
    config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    contracts_config: &ContractsConfig,
    token: &str,
    user_id: &str,
) -> Result {
    tracing::info!("Executing submissions...");

    // Use the decentralized party ID from contracts config (provided via UI)
    let decentralized_party = contracts_config.decentralized_party_id.to_string();
    tracing::debug!("Decentralized party: {decentralized_party}");

    // Step 2: Load all prepared submissions from storage. They were keyed by
    // zero-padded ordinal so `list_artifacts` returns them in their original
    // creation order (matching the previous filename-sorted file scan).
    tracing::info!("Loading prepared submissions...");
    let submission_rows = db
        .list_artifacts(instance_name, artifact_kinds::PREPARED_SUBMISSION)
        .await?;

    if submission_rows.is_empty() {
        anyhow::bail!(
            "No PREPARED_SUBMISSION artifacts found for instance {instance_name} — \
             did PrepareSubmissions run?"
        );
    }

    let mut prepared_submissions: Vec<PrepareSubmissionResponse> =
        Vec::with_capacity(submission_rows.len());
    for (ordinal, payload) in &submission_rows {
        let prepared_sub: PrepareSubmissionResponse =
            utils::read_first_message_from_bytes(payload)?;
        tracing::debug!("Loaded prepared submission ordinal {ordinal}");
        prepared_submissions.push(prepared_sub);
    }

    let num_submissions = prepared_submissions.len();
    tracing::debug!("Loaded {num_submissions} prepared submissions");

    // Step 3: Load all per-peer signature bundles from storage.
    tracing::info!("Loading peer signatures...");
    let signature_rows = db
        .list_artifacts(instance_name, artifact_kinds::SUBMISSION_SIGNATURES)
        .await?;
    tracing::debug!(
        "Found signatures from {count} peer(s)",
        count = signature_rows.len()
    );

    if signature_rows.is_empty() {
        anyhow::bail!(
            "No SUBMISSION_SIGNATURES artifacts found for instance {instance_name} — \
             did SignSubmissions complete?"
        );
    }

    // Each row is `varint(len)||proto` × N messages produced by
    // `sign_submissions`. Decode them per-peer.
    let mut all_signatures: Vec<Vec<CantonSignature>> = Vec::new();
    for (peer_id, payload) in &signature_rows {
        tracing::debug!("Loading signatures from peer {peer_id}");

        let sigs: Vec<CantonSignature> = read_all_messages_from_bytes(payload)?;

        if sigs.len() != num_submissions {
            anyhow::bail!(
                "Expected {num_submissions} signatures from peer {peer_id}, \
                 but found {count}",
                count = sigs.len()
            );
        }

        all_signatures.push(sigs);
    }

    tracing::info!(
        "Loaded signatures from {count} peers",
        count = all_signatures.len()
    );

    // Step 4: Execute each submission
    let token_opt = Some(token.to_string());
    let mut submission_client = utils::create_submission_client(config, token_opt.clone()).await?;

    // Contract ids of everything we create, harvested from each submission's
    // committed transaction response so we can confirm them by id afterwards.
    let mut all_created_contract_ids: Vec<String> = Vec::new();

    for (idx, prepared_response) in prepared_submissions.iter().enumerate() {
        tracing::info!("Executing submission {index}...", index = idx + 1);

        // Collect signatures for this submission from all peers
        let mut signatures_for_submission = Vec::new();
        for peer_sigs in &all_signatures {
            let canton_sig = &peer_sigs[idx];

            // Convert Canton Signature to Ledger API Signature
            // The Ledger API Signature doesn't have signature_delegation field
            let ledger_sig = Signature {
                format: canton_sig.format,
                signature: canton_sig.signature.clone(),
                signed_by: canton_sig.signed_by.clone(),
                signing_algorithm_spec: canton_sig.signing_algorithm_spec,
            };

            signatures_for_submission.push(ledger_sig);
        }

        tracing::debug!(
            "Collected {count} signatures for submission {idx}",
            count = signatures_for_submission.len(),
            idx = idx + 1
        );

        // Debug: Log fingerprints being used in signatures
        for (sig_idx, sig) in signatures_for_submission.iter().enumerate() {
            tracing::debug!(
                "Signature {sig_idx} for submission {idx}: signed_by={signature}",
                sig_idx = sig_idx + 1,
                idx = idx + 1,
                signature = sig.signed_by
            );
        }

        // Build PartySignatures
        let party_signatures = PartySignatures {
            signatures: vec![SinglePartySignatures {
                party: decentralized_party.clone(),
                signatures: signatures_for_submission,
            }],
        };

        // Generate unique submission ID
        let submission_id = Uuid::new_v4().to_string();

        // Execute the submission
        // Note: User ID must match JWT token's "sub" claim
        let execute_request = ExecuteSubmissionAndWaitForTransactionRequest {
            prepared_transaction: prepared_response.prepared_transaction.clone(),
            party_signatures: Some(party_signatures),
            deduplication_period: None, // Use default
            submission_id,
            user_id: user_id.to_string(),
            hashing_scheme_version: prepared_response.hashing_scheme_version,
            min_ledger_time: None,
            transaction_format: None,
        };

        let response = submission_client
            .execute_submission_and_wait_for_transaction(tonic::Request::new(execute_request))
            .await?
            .into_inner();

        // The RPC blocks until the transaction is committed, and its response
        // carries the created events — i.e. the exact contracts this submission
        // produced. Harvest their ids so we can confirm them by id later rather
        // than scanning the whole ACS.
        let created = created_contract_ids(&response.transaction);
        tracing::info!(
            "Submission {index} executed successfully, created {count} contract(s)",
            index = idx + 1,
            count = created.len(),
        );
        all_created_contract_ids.extend(created);
    }

    // Step 5: Confirm the created contracts are visible.
    //
    // `execute_submission_and_wait_for_transaction` already blocks until each
    // transaction is committed, so the contracts exist by this point. We still
    // confirm them, but by looking each one up by id via
    // `EventQueryService.GetEventsByContractId` — a cheap point query — instead
    // of streaming the party's entire active-contract set (which can be millions
    // of contracts and is `O(ledger state)`, not `O(contracts we created)`).
    if all_created_contract_ids.is_empty() {
        // No created events came back (e.g. the submitting party isn't hosted on
        // this node, so an ACS_DELTA transaction is filtered to empty). The RPC
        // success above is itself the commit guarantee, so don't fail here.
        tracing::warn!(
            "Execute responses returned no created events to confirm; \
             relying on execute_submission_and_wait_for_transaction success"
        );
    } else {
        tracing::info!(
            "Confirming {count} created contract(s) by id...",
            count = all_created_contract_ids.len()
        );

        let mut event_query_client = utils::create_event_query_client(config, token_opt).await?;

        let max_attempts = topology_retry_max_attempts();
        let retry_delay = tokio::time::Duration::from_secs(topology_retry_delay_secs());

        // `GetEventsByContractId` filters by party visibility; the decentralized
        // party is a stakeholder on everything it just created.
        let mut filters_by_party = std::collections::HashMap::new();
        filters_by_party.insert(
            decentralized_party.clone(),
            Filters {
                cumulative: vec![CumulativeFilter {
                    identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
                        WildcardFilter {
                            include_created_event_blob: false,
                        },
                    )),
                }],
            },
        );

        for contract_id in &all_created_contract_ids {
            let mut visible = false;

            for attempt in 1..=max_attempts {
                let request = GetEventsByContractIdRequest {
                    contract_id: contract_id.clone(),
                    event_format: Some(EventFormat {
                        filters_by_party: filters_by_party.clone(),
                        filters_for_any_party: None,
                        verbose: false,
                    }),
                };

                match event_query_client
                    .get_events_by_contract_id(tonic::Request::new(request))
                    .await
                {
                    // A populated `created` event means the contract is visible.
                    Ok(response) => {
                        if response.into_inner().created.is_some() {
                            visible = true;
                            break;
                        }
                        tracing::debug!(
                            "Contract {contract_id} not yet visible (attempt {attempt}/{max_attempts})"
                        );
                    }
                    // `CONTRACT_EVENTS_NOT_FOUND` while the index catches up — retry.
                    Err(status) => {
                        tracing::debug!(
                            "Lookup for {contract_id} failed (attempt {attempt}/{max_attempts}): {status}"
                        );
                    }
                }

                if attempt < max_attempts {
                    tokio::time::sleep(retry_delay).await;
                }
            }

            if !visible {
                anyhow::bail!(
                    "Created contract {contract_id} not visible after {max_attempts} attempts"
                );
            }
        }

        tracing::info!(
            "All {count} created contract(s) confirmed visible",
            count = all_created_contract_ids.len()
        );
    }

    tracing::info!("Submissions executed successfully");
    Ok(())
}

/// Collect the contract ids of every `CreatedEvent` in an execute-and-wait
/// transaction response. These are the exact contracts the submission created,
/// letting us confirm them by id rather than scanning the whole ACS. Returns an
/// empty vec when the response carries no transaction or no created events.
fn created_contract_ids(transaction: &Option<Transaction>) -> Vec<String> {
    let Some(tx) = transaction else {
        return Vec::new();
    };

    tx.events
        .iter()
        .filter_map(|ev| {
            ev.event.as_ref().and_then(|inner| match inner {
                event::Event::Created(created) => Some(created.contract_id.clone()),
                _ => None,
            })
        })
        .collect()
}

/// Decode a sequence of `varint(len)||proto` messages from a byte slice. Mirrors
/// `utils::read_all_messages_from_file` but operates on in-memory data — used
/// to round-trip blobs we used to read from disk.
fn read_all_messages_from_bytes<M: prost::Message + Default>(data: &[u8]) -> Result<Vec<M>> {
    let mut cursor = data;
    let mut messages = Vec::new();
    while cursor.has_remaining() {
        let len = prost::encoding::decode_varint(&mut cursor)? as usize;
        if cursor.remaining() < len {
            anyhow::bail!(
                "Incomplete message: expected {len} bytes, but only {remaining} remaining",
                remaining = cursor.remaining()
            );
        }
        let message_bytes = &cursor[..len];
        cursor.advance(len);
        messages.push(M::decode(message_bytes)?);
    }
    Ok(messages)
}
