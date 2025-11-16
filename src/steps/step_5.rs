use crate::network_config::NodeConfig;
use tokio::fs;
use uuid::Uuid;

use crate::{
    consts::{TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS},
    dirs::WorkflowDirs,
    error::Result,
    proto::com::{
        daml::ledger::api::v2::{
            CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
            WildcardFilter, cumulative_filter,
            interactive::{
                ExecuteSubmissionRequest, PartySignatures, PrepareSubmissionResponse,
                SinglePartySignatures,
                interactive_submission_service_client::InteractiveSubmissionServiceClient,
            },
            state_service_client::StateServiceClient,
        },
        digitalasset::canton::{
            crypto::v30::Signature as CantonSignature,
            protocol::v30::DecentralizedNamespaceDefinition,
        },
    },
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
pub async fn execute_submissions(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Executing submissions...");

    // Step 1: Get decentralized party ID from namespace definition
    let namespace_file = dirs.dns_submission_dir.join("namespaceDef.bin");
    tracing::debug!(
        "Reading namespace definition from {}",
        namespace_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_file).await?;

    let decentralized_party = format!("cbtc-network::{}", namespace_def.decentralized_namespace);
    tracing::debug!("Decentralized party: {decentralized_party}");

    // Step 2: Load prepared submissions
    tracing::info!("Loading prepared submissions...");
    let ledger_submissions_dir = dirs.workflow_dir.join("ledger-submissions");
    let prepared_dir = ledger_submissions_dir.join("prepared");

    let prepared_sub1: PrepareSubmissionResponse =
        utils::read_first_message_from_file(&prepared_dir.join("prepared-submission-1.bin"))
            .await?;
    let prepared_sub2: PrepareSubmissionResponse =
        utils::read_first_message_from_file(&prepared_dir.join("prepared-submission-2.bin"))
            .await?;
    let prepared_sub3: PrepareSubmissionResponse =
        utils::read_first_message_from_file(&prepared_dir.join("prepared-submission-3.bin"))
            .await?;

    tracing::debug!("Loaded 3 prepared submissions");

    // Step 3: Discover and load all signature files
    tracing::info!("Loading attestor signatures...");
    let execution_dir = dirs.workflow_dir.join("execution");
    let signatures_dir = execution_dir.join("signatures");

    // Discover all submission-signatures-*.bin files
    let mut signature_files = Vec::new();
    let mut entries = fs::read_dir(&signatures_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("submission-signatures-") && name.ends_with(".bin") {
                    signature_files.push(path);
                }
            }
        }
    }

    signature_files.sort();
    tracing::debug!("Found {} signature files", signature_files.len());

    if signature_files.is_empty() {
        anyhow::bail!("No signature files found in {}", signatures_dir.display());
    }

    // Load all signatures (3 per file, one for each submission)
    let mut all_signatures: Vec<Vec<CantonSignature>> = Vec::new();

    for sig_file in &signature_files {
        tracing::debug!("Loading signatures from {}", sig_file.display());

        let sigs: Vec<CantonSignature> = utils::read_all_messages_from_file(sig_file).await?;

        if sigs.len() != 3 {
            anyhow::bail!(
                "Expected 3 signatures in {}, but found {}",
                sig_file.display(),
                sigs.len()
            );
        }

        all_signatures.push(sigs);
        tracing::debug!(
            "Loaded 3 signatures from {}",
            sig_file.file_name().unwrap().to_string_lossy()
        );
    }

    tracing::info!("Loaded signatures from {} attestors", all_signatures.len());

    // Step 4: Execute each submission
    let mut submission_client =
        InteractiveSubmissionServiceClient::connect(config.ledger_api_url()).await?;

    let prepared_submissions = vec![
        ("create-govR", prepared_sub1),
        ("create-daR", prepared_sub2),
        ("create-waR", prepared_sub3),
    ];

    for (idx, (command_id, prepared_response)) in prepared_submissions.iter().enumerate() {
        tracing::info!("Executing submission {} ({command_id})...", idx + 1);

        // Collect signatures for this submission from all attestors
        let mut signatures_for_submission = Vec::new();
        for attestor_sigs in &all_signatures {
            let canton_sig = &attestor_sigs[idx];

            // Convert Canton Signature to Ledger API Signature
            // The Ledger API Signature doesn't have signature_delegation field
            let ledger_sig = crate::proto::com::daml::ledger::api::v2::Signature {
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
        let execute_request = ExecuteSubmissionRequest {
            prepared_transaction: prepared_response.prepared_transaction.clone(),
            party_signatures: Some(party_signatures),
            deduplication_period: None, // Use default
            submission_id,
            user_id: "CoordinatorUser".to_string(),
            hashing_scheme_version: prepared_response.hashing_scheme_version,
            min_ledger_time: None,
        };

        submission_client
            .execute_submission(tonic::Request::new(execute_request))
            .await?;

        tracing::info!("Submission {} executed successfully", idx + 1);
    }

    // Step 5: Wait for contracts to appear in ACS
    tracing::info!("Waiting for contracts to appear in ledger...");
    let mut state_client = StateServiceClient::connect(config.ledger_api_url()).await?;

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
        let acs_request = GetActiveContractsRequest {
            active_at_offset: ledger_end,
            event_format: Some(EventFormat {
                filters_by_party: std::collections::HashMap::new(),
                filters_for_any_party: Some(Filters {
                    cumulative: vec![CumulativeFilter {
                        identifier_filter: Some(
                            cumulative_filter::IdentifierFilter::WildcardFilter(WildcardFilter {
                                include_created_event_blob: false,
                            }),
                        ),
                    }],
                }),
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

        // We expect at least 3 contracts (GovernanceRules, DepositAccountRules, WithdrawAccountRules)
        if contract_count >= 3 {
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
                "Contracts not visible in ACS after {max_attempts} attempts. Found only {contract_count} contracts, expected at least 3"
            );
        }
    }

    tracing::info!("Submissions executed successfully");
    Ok(())
}
