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
    config::NodeConfig,
    consts::VAULT_GOVERNANCE_PACKAGE_ID,
    error::Result,
    participant_id::CantonId,
    server::{
        AppState, action_serializer,
        queries::{
            get_governance_confirmations, get_governance_state as query_governance_state,
            get_provider_services, get_user_services, get_vaults,
        },
        types::{
            ConfirmActionRequest, ExecuteActionRequest, ExpireConfirmationRequest,
            GovernanceResponse, GovernanceStateResponse, ProviderServicesResponse,
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
    pub party_id: String,
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
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    // Get token for this party
    let token = get_party_token(&data, &party_id).await;

    // Check if we're in test mode (mock auth)
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    // Get threshold for this party (default to 2 if not found)
    let threshold = get_party_threshold(&data, &query.party_id)
        .await
        .unwrap_or(2);

    // Get member_party_id from config (used by frontend to identify own confirmations)
    let member_party_id = get_member_party_id(&data, &party_id);

    match get_governance_confirmations(&data.config, &query.party_id, threshold, token, test_mode)
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
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    // Get token for this party
    let token = get_party_token(&data, &party_id).await;

    // Check if we're in test mode (mock auth)
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match query_governance_state(&data.config, &query.party_id, token, test_mode).await {
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
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    // Get token for this party
    let token = get_party_token(&data, &party_id).await;

    // Check if we're in test mode (mock auth)
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match get_vaults(&data.config, &query.party_id, token, test_mode).await {
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
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    let token = get_party_token(&data, &party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match get_provider_services(&data.config, &query.party_id, token, test_mode).await {
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
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    let token = get_party_token(&data, &party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match get_user_services(&data.config, &query.party_id, token, test_mode).await {
        Ok(services) => HttpResponse::Ok().json(UserServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch user services: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch user services: {e}")
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

    match execute_confirm_action(&data.config, &body, &token, &member_party_id).await {
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

    match execute_confirmed_action(&data.config, &body, &token, &member_party_id).await {
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

    match execute_expire_confirmation(&data.config, &body, &token, &member_party_id).await {
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
) -> Result {
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: VAULT_GOVERNANCE_PACKAGE_ID.to_string(),
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
) -> Result {
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: VAULT_GOVERNANCE_PACKAGE_ID.to_string(),
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
) -> Result {
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: VAULT_GOVERNANCE_PACKAGE_ID.to_string(),
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
