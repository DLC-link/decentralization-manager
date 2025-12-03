use uuid::Uuid;

use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
        Signature, WildcardFilter, cumulative_filter,
        interactive::{
            ExecuteSubmissionAndWaitForTransactionRequest, PartySignatures,
            PrepareSubmissionResponse, SinglePartySignatures,
        },
    },
    digitalasset::canton::{
        crypto::v30::Signature as CantonSignature, protocol::v30::DecentralizedNamespaceDefinition,
    },
};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{
        EXECUTION_DIR, LEDGER_SUBMISSIONS_DIR, NAMESPACE_DEF_FILENAME, PREPARED_DIR,
        PREPARED_SUBMISSION_PREFIX, SIGNATURES_DIR, SUBMISSION_SIGNATURES_PREFIX,
        TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    dirs::WorkflowDirs,
    error::Result,
    utils,
};

/// Execute signed ledger submissions
///
/// Corresponds to: 05_ExecuteSubmissions.sc
///
/// This step must be run by the coordinator with appropriate Ledger API credentials.
/// It aggregates all signatures and executes the prepared submissions on the ledger.
///
/// # Arguments
/// * `config` - Configuration with Ledger API connection details
/// * `dirs` - WorkflowDirs containing all directory paths
/// * `network_config` - Network configuration with application settings
pub async fn execute_submissions(
    config: &NodeConfig,
    dirs: &WorkflowDirs,
    network_config: &NetworkConfig,
) -> Result {
    tracing::info!("Executing submissions...");

    let party_id_prefix = &network_config.application.party_id_prefix;
    let user_id = &config.canton.ledger_api_user_id;

    // Step 1: Get decentralized party ID from namespace definition
    let namespace_file = dirs.dns_submission_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::debug!(
        "Reading namespace definition from {}",
        namespace_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_file).await?;

    let decentralized_party = format!(
        "{party_id_prefix}::{}",
        namespace_def.decentralized_namespace
    );
    tracing::debug!("Decentralized party: {decentralized_party}");

    // Step 2: Dynamically load all prepared submissions
    tracing::info!("Loading prepared submissions...");
    let ledger_submissions_dir = dirs.workflow_dir.join(LEDGER_SUBMISSIONS_DIR);
    let prepared_dir = ledger_submissions_dir.join(PREPARED_DIR);

    // Discover all prepared-submission-*.bin files
    let submission_files =
        utils::find_files_by_pattern(&prepared_dir, PREPARED_SUBMISSION_PREFIX, ".bin").await?;

    if submission_files.is_empty() {
        anyhow::bail!(
            "No prepared submission files found in {}",
            prepared_dir.display()
        );
    }

    // Load all prepared submissions
    let mut prepared_submissions: Vec<PrepareSubmissionResponse> = Vec::new();
    for submission_file in &submission_files {
        let prepared_sub: PrepareSubmissionResponse =
            utils::read_first_message_from_file(submission_file).await?;
        prepared_submissions.push(prepared_sub);
    }

    let num_submissions = prepared_submissions.len();
    tracing::debug!("Loaded {num_submissions} prepared submissions");

    // Step 3: Discover and load all signature files
    tracing::info!("Loading attestor signatures...");
    let execution_dir = dirs.workflow_dir.join(EXECUTION_DIR);
    let signatures_dir = execution_dir.join(SIGNATURES_DIR);

    // Discover all submission-signatures-*.bin files
    let signature_files =
        utils::find_files_by_pattern(&signatures_dir, SUBMISSION_SIGNATURES_PREFIX, ".bin").await?;
    tracing::debug!("Found {} signature files", signature_files.len());

    if signature_files.is_empty() {
        anyhow::bail!("No signature files found in {}", signatures_dir.display());
    }

    // Load all signatures (one per submission per attestor)
    let mut all_signatures: Vec<Vec<CantonSignature>> = Vec::new();

    for sig_file in &signature_files {
        tracing::debug!("Loading signatures from {}", sig_file.display());

        let sigs: Vec<CantonSignature> = utils::read_all_messages_from_file(sig_file).await?;

        if sigs.len() != num_submissions {
            anyhow::bail!(
                "Expected {num_submissions} signatures in {}, but found {}",
                sig_file.display(),
                sigs.len()
            );
        }

        all_signatures.push(sigs);
        tracing::debug!(
            "Loaded {num_submissions} signatures from {}",
            sig_file.file_name().unwrap().to_string_lossy()
        );
    }

    tracing::info!("Loaded signatures from {} attestors", all_signatures.len());

    // Step 4: Execute each submission
    let mut submission_client = utils::create_submission_client(config).await?;

    for (idx, prepared_response) in prepared_submissions.iter().enumerate() {
        tracing::info!("Executing submission {}...", idx + 1);

        // Collect signatures for this submission from all attestors
        let mut signatures_for_submission = Vec::new();
        for attestor_sigs in &all_signatures {
            let canton_sig = &attestor_sigs[idx];

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
            "Collected {} signatures for submission {}",
            signatures_for_submission.len(),
            idx + 1
        );

        // Debug: Log fingerprints being used in signatures
        for (sig_idx, sig) in signatures_for_submission.iter().enumerate() {
            tracing::debug!(
                "Signature {} for submission {}: signed_by={}",
                sig_idx + 1,
                idx + 1,
                sig.signed_by
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

        submission_client
            .execute_submission_and_wait_for_transaction(tonic::Request::new(execute_request))
            .await?;

        tracing::info!("Submission {} executed successfully", idx + 1);
    }

    // Step 5: Wait for contracts to appear in ACS
    tracing::info!("Waiting for contracts to appear in ledger...");
    let mut state_client = utils::create_state_client(config).await?;

    let max_attempts = TOPOLOGY_RETRY_MAX_ATTEMPTS;
    let retry_delay = tokio::time::Duration::from_secs(TOPOLOGY_RETRY_DELAY_SECS);

    for attempt in 1..=max_attempts {
        // Get current ledger end
        let ledger_end = state_client
            .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
            .await?
            .into_inner()
            .offset;

        // Query ACS for the decentralized party
        // Filter by the specific party rather than "any party" to avoid permission issues
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

        let acs_request = GetActiveContractsRequest {
            active_at_offset: ledger_end,
            event_format: Some(EventFormat {
                filters_by_party,
                filters_for_any_party: None,
                verbose: false,
            }),
        };

        let mut stream = state_client
            .get_active_contracts(tonic::Request::new(acs_request))
            .await?
            .into_inner();

        let mut contract_count = 0;
        while let Some(response) = stream.message().await? {
            if response.contract_entry.is_some() {
                contract_count += 1;
            }
        }

        tracing::debug!(
            "Found {contract_count} contracts for party {decentralized_party} (attempt {attempt}/{max_attempts})",
        );

        // We expect at least as many contracts as submissions
        if contract_count >= num_submissions {
            tracing::info!(
                "All contracts successfully created! Found {contract_count} contracts after {attempt} attempt(s)"
            );
            break;
        }

        if attempt < max_attempts {
            tracing::debug!("Contracts not yet visible, retrying in {retry_delay:?}...");
            tokio::time::sleep(retry_delay).await;
        } else {
            anyhow::bail!(
                "Contracts not visible in ACS after {max_attempts} attempts. Found only {contract_count} contracts, expected at least {num_submissions}"
            );
        }
    }

    tracing::info!("Submissions executed successfully");
    Ok(())
}
