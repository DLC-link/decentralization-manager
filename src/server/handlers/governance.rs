use actix_web::{HttpResponse, Responder, get, post, web};
use anyhow::Context;
use base64::Engine;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        Command, Commands, DisclosedContract, ExerciseCommand, Identifier, Record, RecordField,
        SubmitAndWaitRequest, Value, command, command_service_client::CommandServiceClient, value,
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
    config::{NodeConfig, PackageConfig},
    error::Result,
    participant_id::CantonId,
    server::{
        AppState, action_serializer,
        queries::{
            ContractQueryParams as QueryContractParams, get_governance_confirmations,
            get_governance_state as query_governance_state, get_provider_services,
            get_registrar_services, get_user_services, get_vaults, query_contracts_by_template,
        },
        types::{
            CancelConfirmationRequest, ConfirmActionRequest, ContractQueryResponse,
            ContractWithBlob, ExecuteActionRequest, ExpireConfirmationRequest, GovernanceResponse,
            GovernanceStateResponse, ProviderServicesResponse, RegistrarServicesResponse,
            UserServicesResponse, VaultsResponse,
        },
    },
    utils,
};

// ============================================================================
// Query Types
// ============================================================================

/// Query parameters for governance endpoints
#[derive(Debug, Deserialize)]
pub struct GovernanceQuery {
    pub party_id: CantonId,
}

/// Query parameters for generic contract query endpoint
#[derive(Debug, Deserialize)]
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
#[get("/governance/confirmations")]
pub async fn get_governance(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    let threshold = get_party_threshold(&data, &party_id_str).await.unwrap_or(2);

    let member_party_id = get_member_party_id(&data, party_id);
    let packages = data.config.get_packages(&party_id_str);

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
        Ok(actions) => HttpResponse::Ok().json(GovernanceResponse {
            actions,
            threshold,
            member_party_id,
        }),
        Err(e) => {
            tracing::error!("Failed to fetch governance confirmations: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch governance confirmations: {e}")
            }))
        }
    }
}

/// Get governance state (VaultGovernanceRules contract state)
#[get("/governance/state")]
pub async fn get_governance_state(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));
    let packages = data.config.get_packages(&party_id_str);

    match query_governance_state(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(state) => HttpResponse::Ok().json(GovernanceStateResponse { state }),
        Err(e) => {
            tracing::error!("Failed to fetch governance state: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch governance state: {e}")
            }))
        }
    }
}

/// Get deployed Vault contracts
#[get("/vaults")]
pub async fn get_vaults_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));
    let packages = data.config.get_packages(&party_id_str);

    match get_vaults(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(vaults) => HttpResponse::Ok().json(VaultsResponse { vaults }),
        Err(e) => {
            tracing::error!("Failed to fetch vaults: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch vaults: {e}")
            }))
        }
    }
}

/// Get ProviderService contracts
#[get("/services/provider")]
pub async fn get_provider_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));
    let packages = data.config.get_packages(&party_id_str);

    match get_provider_services(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(ProviderServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch provider services: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch provider services: {e}")
            }))
        }
    }
}

/// Get UserService contracts
#[get("/services/user")]
pub async fn get_user_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));
    let packages = data.config.get_packages(&party_id_str);

    match get_user_services(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(UserServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch user services: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch user services: {e}")
            }))
        }
    }
}

/// Get RegistrarService contracts
#[get("/services/registrar")]
pub async fn get_registrar_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));
    let packages = data.config.get_packages(&party_id_str);

    match get_registrar_services(&data.config, &party_id_str, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(RegistrarServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch registrar services: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch registrar services: {e}")
            }))
        }
    }
}

/// Query contract IDs by template
#[get("/contracts/query")]
pub async fn query_contracts_handler(
    data: web::Data<AppState>,
    query: web::Query<ContractQueryParams>,
) -> impl Responder {
    let party_id = &query.party_id;
    let party_id_str = party_id.to_string();

    let token = get_party_token(&data, party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

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
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to query contracts: {e}")
            }))
        }
    }
}

// ============================================================================
// Action Endpoints
// ============================================================================

/// Submit a confirmation for a governance action using structured ActionType
#[post("/governance/confirm")]
pub async fn confirm_action(
    data: web::Data<AppState>,
    body: web::Json<ConfirmActionRequest>,
) -> impl Responder {
    if let Err(msg) = body.action.validate() {
        return HttpResponse::BadRequest().json(serde_json::json!({ "error": msg }));
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "No credentials configured for party"
            }));
        }
    };

    let packages = data.config.get_packages(&body.party_id.to_string());

    match execute_confirm_action(&data.config, &body, &token, &member_party_id, &packages).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "message": "Confirmation submitted successfully"
        })),
        Err(e) => {
            tracing::error!("Failed to submit confirmation: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to submit confirmation: {e}")
            }))
        }
    }
}

/// Execute a confirmed governance action using structured ActionType
#[post("/governance/execute")]
pub async fn execute_action(
    data: web::Data<AppState>,
    body: web::Json<ExecuteActionRequest>,
) -> impl Responder {
    if let Err(msg) = body.action.validate() {
        return HttpResponse::BadRequest().json(serde_json::json!({ "error": msg }));
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "No credentials configured for party"
            }));
        }
    };

    let packages = data.config.get_packages(&body.party_id.to_string());

    match execute_confirmed_action(&data.config, &body, &token, &member_party_id, &packages).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "message": "Action executed successfully"
        })),
        Err(e) => {
            tracing::error!("Failed to execute action: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to execute action: {e}")
            }))
        }
    }
}

/// Expire a stale governance confirmation
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
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "No credentials configured for party"
            }));
        }
    };

    let packages = data.config.get_packages(&body.party_id.to_string());

    match execute_expire_confirmation(&data.config, &body, &token, &member_party_id, &packages)
        .await
    {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "message": "Confirmation expired successfully"
        })),
        Err(e) => {
            tracing::error!("Failed to expire confirmation: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to expire confirmation: {e}")
            }))
        }
    }
}

/// Cancel (revoke) own governance confirmation
#[post("/governance/cancel")]
pub async fn cancel_confirmation(
    data: web::Data<AppState>,
    body: web::Json<CancelConfirmationRequest>,
) -> impl Responder {
    let party_id = &body.party_id;

    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "No credentials configured for party"
            }));
        }
    };

    let packages = data.config.get_packages(&body.party_id.to_string());

    match execute_cancel_confirmation(&data.config, &body, &token, &member_party_id, &packages)
        .await
    {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "message": "Confirmation cancelled successfully"
        })),
        Err(e) => {
            tracing::error!("Failed to cancel confirmation: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to cancel confirmation: {e}")
            }))
        }
    }
}

/// Get package configuration for a party
#[get("/packages")]
pub async fn get_packages(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let packages = data.config.get_packages(&query.party_id.to_string());
    HttpResponse::Ok().json(packages)
}

/// Fetch AmuletRules contract from the DSO API
#[get("/amulet-rules")]
pub async fn get_amulet_rules(data: web::Data<AppState>) -> impl Responder {
    let url = data.config.canton.network.dso_url();
    let client = reqwest::Client::new();

    match client.get(url).send().await {
        Ok(res) if res.status().is_success() => match res.json::<serde_json::Value>().await {
            Ok(json) => {
                let contract_id = json
                    .pointer("/amulet_rules/contract/contract_id")
                    .and_then(|v| v.as_str());
                let blob = json
                    .pointer("/amulet_rules/contract/created_event_blob")
                    .and_then(|v| v.as_str());

                match (contract_id, blob) {
                    (Some(cid), Some(blob)) => HttpResponse::Ok().json(ContractWithBlob {
                        contract_id: cid.to_string(),
                        blob: blob.to_string(),
                    }),
                    _ => {
                        tracing::warn!("Unexpected amulet-rules response format: {json}");
                        HttpResponse::BadGateway().json(serde_json::json!({
                            "error": "Unexpected response format from DSO API"
                        }))
                    }
                }
            }
            Err(e) => HttpResponse::BadGateway().json(serde_json::json!({
                "error": format!("Failed to parse amulet-rules response: {e}")
            })),
        },
        Ok(res) => {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            tracing::error!("DSO API returned {status}: {body}");
            HttpResponse::BadGateway().json(serde_json::json!({
                "error": format!("DSO API returned {status}: {body}")
            }))
        }
        Err(e) => HttpResponse::BadGateway().json(serde_json::json!({
            "error": format!("Failed to reach DSO API: {e}")
        })),
    }
}

/// Proxy request to fetch token standard contracts (avoids CORS)
#[post("/token-standard-contracts")]
pub async fn get_token_standard_contracts(body: web::Json<serde_json::Value>) -> impl Responder {
    let client = reqwest::Client::new();
    let url = "https://devnet.dlc.link/attestor-2/app/get-token-standard-contracts";

    match client.post(url).json(&body.into_inner()).send().await {
        Ok(res) => match res.json::<serde_json::Value>().await {
            Ok(json) => HttpResponse::Ok().json(json),
            Err(e) => HttpResponse::BadGateway().json(serde_json::json!({
                "error": format!("Failed to parse response: {e}")
            })),
        },
        Err(e) => HttpResponse::BadGateway().json(serde_json::json!({
            "error": format!("Failed to fetch token standard contracts: {e}")
        })),
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get token for a party from auth registry
async fn get_party_token(data: &web::Data<AppState>, party_id: &CantonId) -> Option<String> {
    match &data.auth {
        Some(WorkflowAuth::Keycloak(registry)) => registry.get(party_id)?.get_token().await.ok(),
        Some(WorkflowAuth::Mock(mock_registry)) => Some(mock_registry.get(party_id).get_token()),
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
fn get_member_party_id(data: &web::Data<AppState>, party_id: &CantonId) -> Option<String> {
    data.config
        .parties
        .iter()
        .find(|p| &p.dec_party_id == party_id)
        .map(|p| p.member_party_id.to_string())
}

/// Get token and member_party_id for a party
async fn get_party_credentials(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<(String, String)> {
    match &data.auth {
        Some(WorkflowAuth::Keycloak(registry)) => {
            let tm = registry.get(party_id)?;
            let token = tm.get_token().await.ok()?;
            Some((token, tm.member_party_id().to_string()))
        }
        Some(WorkflowAuth::Mock(mock_registry)) => {
            let mm = mock_registry.get(party_id);
            Some((mm.get_token(), mm.member_party_id().to_string()))
        }
        None => None,
    }
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
    let vault_governance_pkg = packages
        .vault_governance
        .as_deref()
        .context("vault_governance package not configured")?;

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: vault_governance_pkg.to_string(),
        module_name: "BitsafeVault.VaultGovernance".to_string(),
        entity_name: "VaultGovernanceRules".to_string(),
    };

    // Build choice argument using action_serializer
    let choice_argument =
        action_serializer::build_confirm_action_argument(member_party_id, &request.action);

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice: "VaultGovernanceRules_ConfirmAction".to_string(),
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

/// Execute ExecuteConfirmedAction choice on VaultGovernanceRules contract with structured action
async fn execute_confirmed_action(
    config: &NodeConfig,
    request: &ExecuteActionRequest,
    token: &str,
    member_party_id: &str,
    packages: &PackageConfig,
) -> Result {
    let vault_governance_pkg = packages
        .vault_governance
        .as_deref()
        .context("vault_governance package not configured")?;

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: vault_governance_pkg.to_string(),
        module_name: "BitsafeVault.VaultGovernance".to_string(),
        entity_name: "VaultGovernanceRules".to_string(),
    };

    // Build choice argument using action_serializer
    let choice_argument = action_serializer::build_execute_action_argument(
        member_party_id,
        &request.action,
        &request.confirmation_cids,
        None, // contractCid is optional, typically None for execute
    );

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice: "VaultGovernanceRules_ExecuteConfirmedAction".to_string(),
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

/// Execute ExpireConfirmation choice on VaultGovernanceRules contract
async fn execute_expire_confirmation(
    config: &NodeConfig,
    request: &ExpireConfirmationRequest,
    token: &str,
    member_party_id: &str,
    packages: &PackageConfig,
) -> Result {
    let vault_governance_pkg = packages
        .vault_governance
        .as_deref()
        .context("vault_governance package not configured")?;

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: vault_governance_pkg.to_string(),
        module_name: "BitsafeVault.VaultGovernance".to_string(),
        entity_name: "VaultGovernanceRules".to_string(),
    };

    // Build choice argument: ExpireConfirmation { member : Party, confirmationCid : ContractId VaultGovernanceConfirmation }
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

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice: "VaultGovernanceRules_ExpireConfirmation".to_string(),
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

/// Execute Cancel choice directly on VaultGovernanceConfirmation contract
async fn execute_cancel_confirmation(
    config: &NodeConfig,
    request: &CancelConfirmationRequest,
    token: &str,
    member_party_id: &str,
    packages: &PackageConfig,
) -> Result {
    let vault_governance_pkg = packages
        .vault_governance
        .as_deref()
        .context("vault_governance package not configured")?;

    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: vault_governance_pkg.to_string(),
        module_name: "BitsafeVault.VaultGovernance".to_string(),
        entity_name: "VaultGovernanceConfirmation".to_string(),
    };

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
            choice: "VaultGovernanceConfirmation_Cancel".to_string(),
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
