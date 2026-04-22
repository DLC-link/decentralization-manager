use actix_web::{HttpResponse, Responder, get, post, web};
use anyhow::Context;
use base64::Engine;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        Command, Commands, CreateCommand, DisclosedContract, ExerciseCommand, Identifier, Record,
        RecordField, SubmitAndWaitForTransactionRequest, SubmitAndWaitRequest, Value, command,
        command_service_client::CommandServiceClient, value,
    },
    digitalasset::canton::topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, StoreId, Synchronizer, base_query,
        store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use serde::Deserialize;

use crate::{
    auth::WorkflowAuth,
    config::{NodeConfig, PackageConfig, default_package_config},
    db::schema::SchemaRead,
    error::Result,
    participant_id::CantonId,
    server::{
        AppState, action_serializer,
        audit::{AuditEvent, AuditParams, spawn_audit_log},
        chain_audit,
        queries::{
            ContractQueryParams as QueryContractParams, get_governance_confirmations,
            get_governance_state as query_governance_state, get_provider_services,
            get_registrar_services, get_user_services, get_vaults, query_contracts_by_template,
        },
        types::{
            AuditLogEntry, AuditLogQuery, AuditLogResponse, CancelConfirmationRequest,
            ChainAuditEntry, ChainAuditQuery, ChainAuditResponse, ConfirmActionRequest,
            ContractQueryResponse, ErrorResponse, ExecuteActionRequest, ExpireConfirmationRequest,
            GovernanceResponse, GovernanceStateResponse, GovernanceType, MessageResponse,
            NetworkInfo, ProposeActionRequest, ProviderServicesResponse, RegistrarServicesResponse,
            UserServicesResponse, VaultsResponse,
        },
    },
    utils,
};

// ============================================================================
// Query Types
// ============================================================================

/// Query parameters for governance endpoints
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct GovernanceQuery {
    pub party_id: CantonId,
}

/// Query parameters for generic contract query endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ContractQueryParams {
    pub party_id: CantonId,
    pub package_id: String,
    pub module_name: String,
    pub entity_name: String,
    /// Use InterfaceFilter instead of TemplateFilter (for querying by interface)
    #[serde(default)]
    pub interface: bool,
}

// ============================================================================
// Read Endpoints
// ============================================================================

/// Get governance confirmations with parsed actions
#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Governance confirmations", body = GovernanceResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/confirmations")]
pub async fn get_governance(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;

    let threshold = get_party_threshold(&data, &party_id_str).await.unwrap_or(2);

    let member_party_id = get_member_party_id(&data, party_id).await;
    let packages = packages();

    match get_governance_confirmations(
        &data.config,
        &party_id_str,
        threshold,
        token,
        test_mode,
        &packages,
    )
    .await
    {
        Ok((actions, domain_actions)) => HttpResponse::Ok().json(GovernanceResponse {
            actions,
            domain_actions,
            threshold,
            member_party_id,
        }),
        Err(e) => {
            tracing::error!("Failed to fetch governance confirmations: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch governance confirmations: {e}"),
            })
        }
    }
}

/// Get governance state (VaultGovernanceRules contract state)
#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Governance state", body = GovernanceStateResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/state")]
pub async fn get_governance_state(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match query_governance_state(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(state) => HttpResponse::Ok().json(GovernanceStateResponse { state }),
        Err(e) => {
            tracing::error!("Failed to fetch governance state: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch governance state: {e}"),
            })
        }
    }
}

/// Get deployed Vault contracts
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Deployed vaults", body = VaultsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/vaults")]
pub async fn get_vaults_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_vaults(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(vaults) => HttpResponse::Ok().json(VaultsResponse { vaults }),
        Err(e) => {
            tracing::error!("Failed to fetch vaults: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch vaults: {e}"),
            })
        }
    }
}

/// Get ProviderService contracts
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Provider services", body = ProviderServicesResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/services/provider")]
pub async fn get_provider_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_provider_services(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(ProviderServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch provider services: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch provider services: {e}"),
            })
        }
    }
}

/// Get UserService contracts
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "User services", body = UserServicesResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/services/user")]
pub async fn get_user_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_user_services(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(UserServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch user services: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch user services: {e}"),
            })
        }
    }
}

/// Get RegistrarService contracts
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Registrar services", body = RegistrarServicesResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/services/registrar")]
pub async fn get_registrar_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_registrar_services(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(RegistrarServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch registrar services: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch registrar services: {e}"),
            })
        }
    }
}

/// Query contract IDs by template
#[utoipa::path(
    tag = "Services",
    params(ContractQueryParams),
    responses(
        (status = 200, description = "Contract query results", body = ContractQueryResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/contracts/query")]
pub async fn query_contracts_handler(
    data: web::Data<AppState>,
    query: web::Query<ContractQueryParams>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;

    let contract_params = QueryContractParams {
        package_id: query.package_id.clone(),
        module_name: query.module_name.clone(),
        entity_name: query.entity_name.clone(),
        use_interface_filter: query.interface,
    };

    match query_contracts_by_template(
        &data.config,
        &party_id_str,
        token,
        test_mode,
        &contract_params,
    )
    .await
    {
        Ok(contracts) => HttpResponse::Ok().json(ContractQueryResponse { contracts }),
        Err(e) => {
            tracing::error!("Failed to query contracts: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to query contracts: {e}"),
            })
        }
    }
}

/// Get paginated governance audit trail
#[utoipa::path(
    tag = "Governance",
    params(AuditLogQuery),
    responses(
        (status = 200, description = "Governance audit entries", body = AuditLogResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/audit")]
pub async fn get_governance_audit(
    data: web::Data<AppState>,
    query: web::Query<AuditLogQuery>,
) -> impl Responder {
    match data
        .db
        .get_governance_audit(&query.party_id, query.limit, query.offset)
        .await
    {
        Ok(rows) => {
            let entries: Vec<AuditLogEntry> = rows
                .into_iter()
                .map(|row| AuditLogEntry {
                    id: row.id,
                    timestamp: row.timestamp,
                    event_type: row.event_type,
                    party_id: row.party_id,
                    member_party_id: row.member_party_id,
                    governance_type: row.governance_type,
                    action_summary: row.action_summary,
                    details: serde_json::from_str(&row.details)
                        .unwrap_or(serde_json::Value::String(row.details)),
                    status: row.status,
                    error_message: row.error_message,
                    created_at: row.created_at,
                })
                .collect();
            let total_returned = entries.len();
            HttpResponse::Ok().json(AuditLogResponse {
                entries,
                total_returned,
            })
        }
        Err(e) => {
            tracing::error!("Failed to fetch governance audit: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch governance audit: {e}"),
            })
        }
    }
}

/// Get on-chain governance audit entries.
/// Returns cached data by default. Pass `refresh=true` to fetch from Canton and update cache.
#[utoipa::path(
    tag = "Governance",
    params(ChainAuditQuery),
    responses(
        (status = 200, description = "On-chain governance audit entries", body = ChainAuditResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/chain-audit")]
pub async fn get_governance_chain_audit(
    data: web::Data<AppState>,
    query: web::Query<ChainAuditQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    if !query.refresh {
        // Return from cache
        match data
            .db
            .get_chain_audit_cache(&party_id_str, query.limit as i64)
            .await
        {
            Ok(rows) => {
                let entries: Vec<ChainAuditEntry> = rows.into_iter().map(Into::into).collect();
                let total_returned = entries.len();
                return HttpResponse::Ok().json(ChainAuditResponse {
                    entries,
                    total_returned,
                });
            }
            Err(e) => {
                tracing::warn!("Failed to read chain audit cache: {e}");
                // Fall through to live query
            }
        }
    }

    // Fetch from Canton
    let token = get_party_token(&data, party_id).await;
    let pkgs = packages();

    match chain_audit::get_chain_audit(&data.config, &party_id_str, token, &pkgs, query.limit).await
    {
        Ok(entries) => {
            // Save to cache in background
            let pool = data.db.clone();
            let pid = party_id_str.clone();
            let cached = entries.clone();
            tokio::spawn(async move {
                chain_audit::save_chain_audit_cache(&pool, &pid, &cached).await;
            });

            let total_returned = entries.len();
            HttpResponse::Ok().json(ChainAuditResponse {
                entries,
                total_returned,
            })
        }
        Err(e) => {
            tracing::error!("Failed to fetch chain audit for {party_id_str}: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to query chain audit: {e}"),
            })
        }
    }
}

// ============================================================================
// Action Endpoints
// ============================================================================

/// Propose a domain governance action (creates a GovernableAction proposal contract)
#[utoipa::path(
    tag = "Governance",
    request_body = ProposeActionRequest,
    responses(
        (status = 200, description = "Proposal created", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/propose")]
pub async fn propose_action(
    data: web::Data<AppState>,
    body: web::Json<ProposeActionRequest>,
) -> impl Responder {
    let party_id = &body.party_id;
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_summary = crate::server::audit::proposal_summary(&body.proposal);
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.to_string();
    let audit_member = member_party_id.clone();

    let packages = packages();

    let (package_source, module_name, entity_name, create_args) =
        action_serializer::build_proposal_create_args(
            &party_id.to_string(),
            &member_party_id,
            &body.proposal,
        );

    let package_id = match package_source {
        action_serializer::ProposalPackage::GovernanceCore => {
            match packages.governance_core.as_deref() {
                Some(pkg) => pkg,
                None => {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: "governance_core package not configured".to_string(),
                    });
                }
            }
        }
        action_serializer::ProposalPackage::GovernanceTokenCustody => {
            match packages.governance_token_custody.as_deref() {
                Some(pkg) => pkg,
                None => {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: "governance_token_custody package not configured".to_string(),
                    });
                }
            }
        }
        action_serializer::ProposalPackage::GovernanceTokenIssuance => {
            match packages.governance_token_issuance.as_deref() {
                Some(pkg) => pkg,
                None => {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: "governance_token_issuance package not configured".to_string(),
                    });
                }
            }
        }
    };

    let template_id = Identifier {
        package_id: package_id.to_string(),
        module_name: module_name.to_string(),
        entity_name: entity_name.to_string(),
    };

    let cmd = Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some(template_id),
            create_arguments: Some(create_args),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.clone()],
        read_as: vec![party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let channel = match tonic::transport::Channel::from_shared(data.config.ledger_api_url()) {
        Ok(endpoint) => match endpoint.connect().await {
            Ok(ch) => ch,
            Err(e) => {
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: format!("Failed to connect to ledger API: {e}"),
                });
            }
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Invalid ledger API URL: {e}"),
            });
        }
    };

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    // Step 1: Create the proposal and get the contract ID back
    let mut create_req = tonic::Request::new(SubmitAndWaitForTransactionRequest {
        commands: Some(commands),
        transaction_format: None,
    });
    create_req
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    let proposal_cid = match client.submit_and_wait_for_transaction(create_req).await {
        Ok(response) => {
            // Extract created contract ID from the transaction events
            match response.into_inner().transaction.and_then(|tx| {
                tx.events.iter().find_map(|event| {
                    event.event.as_ref().and_then(|e| match e {
                        canton_proto_rs::com::daml::ledger::api::v2::event::Event::Created(
                            created,
                        ) => Some(created.contract_id.clone()),
                        _ => None,
                    })
                })
            }) {
                Some(cid) => cid,
                None => {
                    return HttpResponse::InternalServerError().json(ErrorResponse {
                        error: "Proposal created but could not extract contract ID".to_string(),
                    });
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to create proposal: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Propose,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("Failed to create proposal: {e}")),
                },
            );
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to create proposal: {e}"),
            });
        }
    };

    tracing::info!("Proposal created with CID: {proposal_cid}");

    // Step 2: Immediately confirm the proposal as the proposer
    let governance_core_pkg = match packages.governance_core.as_deref() {
        Some(pkg) => pkg,
        None => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "governance_core package not configured".to_string(),
            });
        }
    };

    let confirm_template = Identifier {
        package_id: governance_core_pkg.to_string(),
        module_name: "Governance.Rules".to_string(),
        entity_name: "GovernanceRules".to_string(),
    };

    let confirm_arg =
        action_serializer::build_confirm_domain_action_arg(&member_party_id, &proposal_cid);

    let confirm_cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(confirm_template),
            contract_id: body.rules_contract_id.clone(),
            choice: "GovernanceRules_ConfirmAction".to_string(),
            choice_argument: Some(confirm_arg),
        })),
    };

    let confirm_commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![confirm_cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.clone()],
        read_as: vec![party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut confirm_req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(confirm_commands),
    });
    confirm_req
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    match client.submit_and_wait(confirm_req).await {
        Ok(_) => {
            tracing::info!("Proposal {proposal_cid} confirmed by proposer");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Propose,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Proposal created and confirmed".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Proposal created but confirmation failed: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Propose,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("Proposal created but confirmation failed: {e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!(
                    "Proposal created (CID: {proposal_cid}) but confirmation failed: {e}"
                ),
            })
        }
    }
}

/// Submit a confirmation for a governance action using structured ActionType
#[utoipa::path(
    tag = "Governance",
    request_body = ConfirmActionRequest,
    responses(
        (status = 200, description = "Confirmation submitted", body = MessageResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/confirm")]
pub async fn confirm_action(
    data: web::Data<AppState>,
    body: web::Json<ConfirmActionRequest>,
) -> impl Responder {
    if let Err(msg) = body.action.validate() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: msg.to_string(),
        });
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_summary = crate::server::audit::action_summary(&body.action);
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.to_string();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    match execute_confirm_action(&data.config, &body, &token, &member_party_id, &packages).await {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Confirm,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Confirmation submitted successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to submit confirmation: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Confirm,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to submit confirmation: {e}"),
            })
        }
    }
}

/// Execute a confirmed governance action using structured ActionType
#[utoipa::path(
    tag = "Governance",
    request_body = ExecuteActionRequest,
    responses(
        (status = 200, description = "Action executed", body = MessageResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/execute")]
pub async fn execute_action(
    data: web::Data<AppState>,
    body: web::Json<ExecuteActionRequest>,
) -> impl Responder {
    if let Err(msg) = body.action.validate() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: msg.to_string(),
        });
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_summary = crate::server::audit::action_summary(&body.action);
    // Redact disclosed contract blobs (can be very large) before storing in audit
    let mut redacted = body.clone();
    for dc in &mut redacted.disclosed_contracts {
        dc.blob = format!("<{} bytes>", dc.blob.len());
    }
    let audit_details = serde_json::to_string(&redacted).unwrap_or_default();
    let audit_party_id = party_id.to_string();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    match execute_confirmed_action(&data.config, &body, &token, &member_party_id, &packages).await {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Execute,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Action executed successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to execute action: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Execute,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to execute action: {e}"),
            })
        }
    }
}

/// Expire a stale governance confirmation
#[utoipa::path(
    tag = "Governance",
    request_body = ExpireConfirmationRequest,
    responses(
        (status = 200, description = "Confirmation expired", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/expire")]
pub async fn expire_confirmation(
    data: web::Data<AppState>,
    body: web::Json<ExpireConfirmationRequest>,
) -> impl Responder {
    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.to_string();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    match execute_expire_confirmation(&data.config, &body, &token, &member_party_id, &packages)
        .await
    {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Expire,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "expire_confirmation".to_string(),
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Confirmation expired successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to expire confirmation: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Expire,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "expire_confirmation".to_string(),
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to expire confirmation: {e}"),
            })
        }
    }
}

/// Cancel (revoke) own governance confirmation
#[utoipa::path(
    tag = "Governance",
    request_body = CancelConfirmationRequest,
    responses(
        (status = 200, description = "Confirmation cancelled", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/cancel")]
pub async fn cancel_confirmation(
    data: web::Data<AppState>,
    body: web::Json<CancelConfirmationRequest>,
) -> impl Responder {
    let party_id = &body.party_id;

    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.to_string();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    match execute_cancel_confirmation(&data.config, &body, &token, &member_party_id, &packages)
        .await
    {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Cancel,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "cancel_confirmation".to_string(),
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Confirmation cancelled successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to cancel confirmation: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Cancel,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "cancel_confirmation".to_string(),
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to cancel confirmation: {e}"),
            })
        }
    }
}

/// Get package configuration for a party
#[utoipa::path(
    tag = "Configuration",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Package configuration", body = PackageConfig)
    )
)]
#[get("/packages")]
pub async fn get_packages() -> impl Responder {
    HttpResponse::Ok().json(packages())
}

/// Get DSO network info (DSO party ID + amulet rules contract)
#[utoipa::path(
    tag = "Proxy",
    responses(
        (status = 200, description = "Network info", body = NetworkInfo),
        (status = 502, description = "DSO API error", body = ErrorResponse)
    )
)]
#[get("/network-info")]
pub async fn get_network_info(data: web::Data<AppState>) -> impl Responder {
    let url = data.config.canton.network.dso_url();
    let client = reqwest::Client::new();

    match client.get(url).send().await {
        Ok(res) if res.status().is_success() => match res.json::<serde_json::Value>().await {
            Ok(json) => {
                let dso_party = json.pointer("/dso_party_id").and_then(|v| v.as_str());
                let contract_id = json
                    .pointer("/amulet_rules/contract/contract_id")
                    .and_then(|v| v.as_str());
                let blob = json
                    .pointer("/amulet_rules/contract/created_event_blob")
                    .and_then(|v| v.as_str());

                match (dso_party, contract_id, blob) {
                    (Some(dso), Some(cid), Some(blob)) => match dso.parse::<CantonId>() {
                        Ok(dso_id) => HttpResponse::Ok().json(NetworkInfo {
                            dso_party_id: dso_id,
                            amulet_rules_cid: cid.to_string(),
                            amulet_rules_blob: blob.to_string(),
                        }),
                        Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
                            error: format!("Invalid DSO party ID: {e}"),
                        }),
                    },
                    _ => {
                        tracing::warn!("Unexpected DSO API response format");
                        HttpResponse::BadGateway().json(ErrorResponse {
                            error: "Unexpected response format from DSO API".to_string(),
                        })
                    }
                }
            }
            Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Failed to parse DSO response: {e}"),
            }),
        },
        Ok(res) => {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            tracing::error!("DSO API returned {status}: {body}");
            HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("DSO API returned {status}: {body}"),
            })
        }
        Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
            error: format!("Failed to reach DSO API: {e}"),
        }),
    }
}

/// Proxy request to fetch token standard contracts (avoids CORS)
#[utoipa::path(
    tag = "Proxy",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Token standard contracts"),
        (status = 502, description = "Bad gateway", body = ErrorResponse)
    )
)]
#[post("/token-standard-contracts")]
pub async fn get_token_standard_contracts(body: web::Json<serde_json::Value>) -> impl Responder {
    let client = reqwest::Client::new();
    let url = "https://devnet.dlc.link/attestor-2/app/get-token-standard-contracts";

    match client.post(url).json(&body.into_inner()).send().await {
        Ok(res) => match res.json::<serde_json::Value>().await {
            Ok(json) => HttpResponse::Ok().json(json),
            Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Failed to parse response: {e}"),
            }),
        },
        Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
            error: format!("Failed to fetch token standard contracts: {e}"),
        }),
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get token for a party from auth registry
async fn get_party_token(data: &web::Data<AppState>, party_id: &CantonId) -> Option<String> {
    let auth = data.auth.read().await;
    match &*auth {
        Some(WorkflowAuth::Keycloak(registry)) => registry.get(party_id)?.get_token().await.ok(),
        Some(WorkflowAuth::Mock(mock_registry)) => {
            Some(mock_registry.get(party_id).await.get_token())
        }
        None => None,
    }
}

/// Get threshold for a decentralized party
async fn get_party_threshold(data: &web::Data<AppState>, party_id: &str) -> Option<usize> {
    // Extract namespace from party_id
    let namespace = party_id.rsplit_once("::")?.1;

    let channel = tonic::transport::Channel::from_shared(data.config.admin_api_url())
        .ok()?
        .connect()
        .await
        .ok()?;

    let mut topology_client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let synchronizer_id = utils::get_synchronizer_id(&data.config).await.ok()?;

    let response = topology_client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(BaseQuery {
                    store: Some(StoreId {
                        store: Some(store_id::Store::Synchronizer(Synchronizer {
                            kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
                        })),
                    }),
                    proposals: false,
                    operation: 0,
                    time_query: Some(base_query::TimeQuery::HeadState(())),
                    filter_signed_key: String::new(),
                    protocol_version: None,
                }),
                filter_namespace: namespace.to_string(),
            },
        ))
        .await
        .ok()?
        .into_inner();

    response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .map(|item| item.threshold as usize)
}

/// Get member_party_id for a decentralized party from config
async fn get_member_party_id(data: &web::Data<AppState>, party_id: &CantonId) -> Option<String> {
    let party_creds = data.party_credentials.read().await;
    party_creds
        .iter()
        .find(|p| &p.dec_party_id == party_id)
        .map(|p| p.member_party_id.to_string())
}

/// Get token and member_party_id for a party
async fn get_party_credentials(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<(String, String)> {
    let auth = data.auth.read().await;
    match &*auth {
        Some(WorkflowAuth::Keycloak(registry)) => {
            let tm = registry.get(party_id)?;
            let token = tm.get_token().await.ok()?;
            Some((token, tm.member_party_id().to_string()))
        }
        Some(WorkflowAuth::Mock(mock_registry)) => {
            let mm = mock_registry.get(party_id).await;
            Some((mm.get_token(), mm.member_party_id().to_string()))
        }
        None => None,
    }
}

/// Get the hardcoded default package config (package IDs are constants, not per-party)
fn packages() -> PackageConfig {
    default_package_config()
}

// ============================================================================
// Ledger Command Execution
// ============================================================================

/// Execute ConfirmAction choice on VaultGovernanceRules contract with structured action
async fn execute_confirm_action(
    config: &NodeConfig,
    request: &ConfirmActionRequest,
    token: &str,
    member_party_id: &str,
    packages: &PackageConfig,
) -> Result {
    let (template_id, choice, choice_argument) = match request.governance_type {
        GovernanceType::Vault => {
            let pkg = packages
                .vault_governance
                .as_deref()
                .context("vault_governance package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "BitsafeVault.VaultGovernance".to_string(),
                    entity_name: "VaultGovernanceRules".to_string(),
                },
                "VaultGovernanceRules_ConfirmAction".to_string(),
                action_serializer::build_confirm_action_argument(member_party_id, &request.action),
            )
        }
        GovernanceType::CoreSelf => {
            let pkg = packages
                .governance_core
                .as_deref()
                .context("governance_core package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "Governance.Rules".to_string(),
                    entity_name: "GovernanceRules".to_string(),
                },
                "GovernanceRules_ConfirmGovernanceAction".to_string(),
                action_serializer::build_confirm_governance_action_arg(
                    member_party_id,
                    &request.action,
                ),
            )
        }
        GovernanceType::CoreDomain => {
            let pkg = packages
                .governance_core
                .as_deref()
                .context("governance_core package not configured")?;
            let proposal_cid = request
                .proposal_cid
                .as_deref()
                .context("proposal_cid required for core_domain confirm")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "Governance.Rules".to_string(),
                    entity_name: "GovernanceRules".to_string(),
                },
                "GovernanceRules_ConfirmAction".to_string(),
                action_serializer::build_confirm_domain_action_arg(member_party_id, proposal_cid),
            )
        }
    };

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice,
            choice_argument: Some(choice_argument),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.to_string()],
        read_as: vec![request.party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Execute ExecuteConfirmedAction choice on governance rules contract
async fn execute_confirmed_action(
    config: &NodeConfig,
    request: &ExecuteActionRequest,
    token: &str,
    member_party_id: &str,
    packages: &PackageConfig,
) -> Result {
    let (template_id, choice, choice_argument) = match request.governance_type {
        GovernanceType::Vault => {
            let pkg = packages
                .vault_governance
                .as_deref()
                .context("vault_governance package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "BitsafeVault.VaultGovernance".to_string(),
                    entity_name: "VaultGovernanceRules".to_string(),
                },
                "VaultGovernanceRules_ExecuteConfirmedAction".to_string(),
                action_serializer::build_execute_action_argument(
                    member_party_id,
                    &request.action,
                    &request.confirmation_cids,
                    None,
                ),
            )
        }
        GovernanceType::CoreSelf => {
            let pkg = packages
                .governance_core
                .as_deref()
                .context("governance_core package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "Governance.Rules".to_string(),
                    entity_name: "GovernanceRules".to_string(),
                },
                "GovernanceRules_ExecuteGovernanceAction".to_string(),
                action_serializer::build_execute_governance_action_arg(
                    member_party_id,
                    &request.action,
                    &request.confirmation_cids,
                ),
            )
        }
        GovernanceType::CoreDomain => {
            let pkg = packages
                .governance_core
                .as_deref()
                .context("governance_core package not configured")?;
            let proposal_cid = request
                .proposal_cid
                .as_deref()
                .context("proposal_cid required for core_domain execute")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "Governance.Rules".to_string(),
                    entity_name: "GovernanceRules".to_string(),
                },
                "GovernanceRules_ExecuteConfirmedAction".to_string(),
                action_serializer::build_execute_domain_action_arg(
                    member_party_id,
                    proposal_cid,
                    &request.confirmation_cids,
                ),
            )
        }
    };

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice,
            choice_argument: Some(choice_argument),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.to_string()],
        read_as: vec![request.party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: request
            .disclosed_contracts
            .iter()
            .map(|dc| {
                Ok(DisclosedContract {
                    template_id: None,
                    contract_id: dc.contract_id.clone(),
                    created_event_blob: base64::engine::general_purpose::STANDARD
                        .decode(&dc.blob)
                        .context("Invalid base64 in disclosed contract blob")?,
                    synchronizer_id: String::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Execute ExpireConfirmation choice on governance rules contract
async fn execute_expire_confirmation(
    config: &NodeConfig,
    request: &ExpireConfirmationRequest,
    token: &str,
    member_party_id: &str,
    packages: &PackageConfig,
) -> Result {
    // Both vault and core use the same argument shape: { member, staleConfirmationCid }
    let choice_argument = Value {
        sum: Some(value::Sum::Record(Record {
            record_id: None,
            fields: vec![
                RecordField {
                    label: "member".to_string(),
                    value: Some(Value {
                        sum: Some(value::Sum::Party(member_party_id.to_string())),
                    }),
                },
                RecordField {
                    label: "staleConfirmationCid".to_string(),
                    value: Some(Value {
                        sum: Some(value::Sum::ContractId(request.confirmation_cid.clone())),
                    }),
                },
            ],
        })),
    };

    let (template_id, choice) = match request.governance_type {
        GovernanceType::Vault => {
            let pkg = packages
                .vault_governance
                .as_deref()
                .context("vault_governance package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "BitsafeVault.VaultGovernance".to_string(),
                    entity_name: "VaultGovernanceRules".to_string(),
                },
                "VaultGovernanceRules_ExpireConfirmation".to_string(),
            )
        }
        GovernanceType::CoreSelf => {
            let pkg = packages
                .governance_core
                .as_deref()
                .context("governance_core package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "Governance.Rules".to_string(),
                    entity_name: "GovernanceRules".to_string(),
                },
                "GovernanceRules_ExpireGovernanceConfirmation".to_string(),
            )
        }
        GovernanceType::CoreDomain => {
            anyhow::bail!("Core domain actions not yet supported for expire")
        }
    };

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice,
            choice_argument: Some(choice_argument),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.to_string()],
        read_as: vec![request.party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Execute Cancel choice directly on a confirmation contract
async fn execute_cancel_confirmation(
    config: &NodeConfig,
    request: &CancelConfirmationRequest,
    token: &str,
    member_party_id: &str,
    packages: &PackageConfig,
) -> Result {
    let (template_id, choice) = match request.governance_type {
        GovernanceType::Vault => {
            let pkg = packages
                .vault_governance
                .as_deref()
                .context("vault_governance package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "BitsafeVault.VaultGovernance".to_string(),
                    entity_name: "VaultGovernanceConfirmation".to_string(),
                },
                "VaultGovernanceConfirmation_Cancel".to_string(),
            )
        }
        GovernanceType::CoreSelf => {
            let pkg = packages
                .governance_core
                .as_deref()
                .context("governance_core package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "Governance.Rules".to_string(),
                    entity_name: "GovernanceSelfConfirmation".to_string(),
                },
                "GovernanceSelfConfirmation_Cancel".to_string(),
            )
        }
        GovernanceType::CoreDomain => {
            anyhow::bail!("Core domain actions not yet supported for cancel")
        }
    };

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    // Cancel takes no arguments
    let choice_argument = Value {
        sum: Some(value::Sum::Record(Record {
            record_id: None,
            fields: vec![],
        })),
    };

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.confirmation_cid.clone(),
            choice,
            choice_argument: Some(choice_argument),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.to_string()],
        read_as: vec![request.party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}
