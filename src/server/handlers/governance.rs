use actix_web::{HttpResponse, Responder, get, post, web};
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
        ActionType, AppState, action_serializer,
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

macro_rules! disclose {
    ($contract_id:expr, $blob:expr) => {
        DisclosedContract {
            template_id: None,
            contract_id: $contract_id.clone(),
            created_event_blob: base64::engine::general_purpose::STANDARD
                .decode($blob)
                .expect("Invalid base64 blob"),
            synchronizer_id: String::new(),
        }
    };
}

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

    match get_governance_confirmations(&data.config, &query.party_id, threshold, token, test_mode)
        .await
    {
        Ok(actions) => HttpResponse::Ok().json(GovernanceResponse { actions, threshold }),
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
        disclosed_contracts: build_disclosed_contracts(&request.action),
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

/// Build disclosed contracts for specific action types
fn build_disclosed_contracts(action: &ActionType) -> Vec<DisclosedContract> {
    match action {
        ActionType::DevNetFeatureApp { amulet_rules_cid } => vec![disclose!(
            amulet_rules_cid,
            "CgMyLjESsQ4KRQCeWuQZiFi479pjMwp/p+/f0LM0lXW6CP/BNOhgF02MX8oSEiD6tnb4BtBzehw1XkegzSaSyNKmuNlguE/Jog0G0eM9VRINc3BsaWNlLWFtdWxldBpkCkAzY2ExMzQzYWIyNmI0NTNkMzhjOGFkYjcwZGNhNWYxZWFkODQ0MGM0MmI1OWI2OGYwNzA3ODY5NTVjYmY5ZWMxEgZTcGxpY2USC0FtdWxldFJ1bGVzGgtBbXVsZXRSdWxlcyLyC2rvCwpNCks6SURTTzo6MTIyMGJlNThjMjllNjVkZTQwYmYyNzNiZTFkYzJiMjY2ZDQzYTlhMDAyZWE1YjE4OTU1YWVlZjdhYWM4ODFiYjQ3MWEKlwsKlAtqkQsKiAsKhQtqggsKkgEKjwFqjAEKFgoUahIKEAoOMgwwLjAwMDAwMDAwMDAKFgoUahIKEAoOMgwwLjAwMDAxOTAyNTkKHAoaahgKEAoOMgwwLjAwMDAwMDAwMDAKBAoCWgAKFgoUahIKEAoOMgwwLjAwMDAwMDAwMDAKEAoOMgwxLjAwMDAwMDAwMDAKBQoDGMgBCgUKAxjIAQoECgIYZArhBgreBmrbBgqYAQqVAWqSAQoaChgyFjQwMDAwMDAwMDAwLjAwMDAwMDAwMDAKEAoOMgwwLjA1MDAwMDAwMDAKEAoOMgwwLjE1MDAwMDAwMDAKEAoOMgwwLjIwMDAwMDAwMDAKFAoSMhAyMDAwMC4wMDAwMDAwMDAwChAKDjIMMC42MDAwMDAwMDAwChYKFFISChAyDjU3MC4wMDAwMDAwMDAwCr0FCroFWrcFCq4BaqsBChAKDmoMCgoKCBiAwM/g6JUHCpYBCpMBapABChoKGDIWMjAwMDAwMDAwMDAuMDAwMDAwMDAwMAoQCg4yDDAuMTIwMDAwMDAwMAoQCg4yDDAuNDAwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoUChIyEDIwMDAwLjAwMDAwMDAwMDAKEAoOMgwwLjYwMDAwMDAwMDAKFAoSUhAKDjIMMy4zMzAwMDAwMDAwCqoBaqcBChAKDmoMCgoKCBiAwO6husEVCpIBCo8BaowBChoKGDIWMTAwMDAwMDAwMDAuMDAwMDAwMDAwMAoQCg4yDDAuMTgwMDAwMDAwMAoQCg4yDDAuNjIwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoQCg4yDDEuNTAwMDAwMDAwMAoQCg4yDDAuNjAwMDAwMDAwMAoUChJSEAoOMgwzLjMzMDAwMDAwMDAKqQFqpgEKEAoOagwKCgoIGICAm8aX2kcKkQEKjgFqiwEKGQoXMhU1MDAwMDAwMDAwLjAwMDAwMDAwMDAKEAoOMgwwLjIxMDAwMDAwMDAKEAoOMgwwLjY5MDAwMDAwMDAKEAoOMgwwLjIwMDAwMDAwMDAKEAoOMgwxLjUwMDAwMDAwMDAKEAoOMgwwLjYwMDAwMDAwMDAKFAoSUhAKDjIMMy4zMzAwMDAwMDAwCqoBaqcBChEKD2oNCgsKCRiAgLaMr7SPAQqRAQqOAWqLAQoZChcyFTI1MDAwMDAwMDAuMDAwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoQCg4yDDAuNzUwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoQCg4yDDEuNTAwMDAwMDAwMAoQCg4yDDAuNjAwMDAwMDAwMAoUChJSEAoOMgwzLjMzMDAwMDAwMDAKjQIKigJqhwIKZwplamMKYQpfYl0KWwpVQlNnbG9iYWwtZG9tYWluOjoxMjIwYmU1OGMyOWU2NWRlNDBiZjI3M2JlMWRjMmIyNjZkNDNhOWEwMDJlYTViMTg5NTVhZWVmN2FhYzg4MWJiNDcxYRICCgAKVwpVQlNnbG9iYWwtZG9tYWluOjoxMjIwYmU1OGMyOWU2NWRlNDBiZjI3M2JlMWRjMmIyNjZkNDNhOWEwMDJlYTViMTg5NTVhZWVmN2FhYzg4MWJiNDcxYQpDCkFqPwocChpqGAoGCgQYgOowCg4KDGoKCggKBhiAsLT4CAoRCg8yDTYwLjAwMDAwMDAwMDAKBAoCGAgKBgoEGIC1GAoOCgxqCgoICgYYgJiavAQKSwpJakcKCgoIQgYwLjEuMTQKCgoIQgYwLjEuMTUKCgoIQgYwLjEuMjAKCQoHQgUwLjEuNQoKCghCBjAuMS4xNAoKCghCBjAuMS4xNAoECgJSAAoUChJSEAoOMgwxLjAwMDAwMDAwMDAKBAoCWgAKBAoCEAEqSURTTzo6MTIyMGJlNThjMjllNjVkZTQwYmYyNzNiZTFkYzJiMjY2ZDQzYTlhMDAyZWE1YjE4OTU1YWVlZjdhYWM4ODFiYjQ3MWE5tgHghutIBgBCKgomCiQIARIg2Md4zgUbR/RJCvIxvewOt7EiYUX/d9m6BxFNwoaw4CYQHg=="
        )],
        ActionType::VaultDeployment {
            vault_rules_cid, ..
        } => vec![disclose!(
            vault_rules_cid,
            "CgMyLjESjwYKRQCMHFjbebg9gs894ojdp4mvHetxBGZJlb4bsG31+th3w8oSEiCYneqOgmkJLin8CWoocGruD6KiiDGIKysaY7nbvnC2LBIUYml0c2FmZS12YXVsdC12MC1yYzIaaApAODkyZjdiNjRkMzgxNDI5N2ZhNTMwYjBhMDgzZDMwZjRiMDY0OWFlOTEzMzY0NTk3NDBlN2M2M2RjMGYyNmYyZBIMQml0c2FmZVZhdWx0EgpWYXVsdFJ1bGVzGgpWYXVsdFJ1bGVzIpICao8CClcKVTpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EKswEKsAFarQEKUjpQdmF1bHQtdGVzdDo6MTIyMGQxMmI4YTRlMmY0NDBkODZmNDY3NjRhZmNmYzhkOTkwOGRlMzVmOTBiMzZiYzE5ZWZjZDhlMTk4NjA2Yzk0ZDIKVzpVdmF1bHQtbWFuYWdlci0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YSpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EyVXZhdWx0LW1hbmFnZXItMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EyUHZhdWx0LXRlc3Q6OjEyMjBkMTJiOGE0ZTJmNDQwZDg2ZjQ2NzY0YWZjZmM4ZDk5MDhkZTM1ZjkwYjM2YmMxOWVmY2Q4ZTE5ODYwNmM5NGQyOdjHx9KSSQYAQioKJgokCAESIG1VKCv3bAsT+BjxtNifG0ZEndE86Q7i1nMXyUktduQSEB4="
        )],
        ActionType::ProcessorDeploymentRequest {
            vault_processor_rules_cid,
            allocation_factory_cid,
            ..
        } => vec![
            disclose!(
                vault_processor_rules_cid,
                "CgMyLjESgAUKRQADHdZlCEAiG5xShFOFTfCSepv/7WjbeNMgvLwKJrb9RcoSEiC8H2AVna89+k+JxAm3r6A92wsYLM7a15KpZOI81rjSihIUYml0c2FmZS12YXVsdC12MC1yYzIaegpAODkyZjdiNjRkMzgxNDI5N2ZhNTMwYjBhMDgzZDMwZjRiMDY0OWFlOTEzMzY0NTk3NDBlN2M2M2RjMGYyNmYyZBIMQml0c2FmZVZhdWx0EhNWYXVsdFByb2Nlc3NvclJ1bGVzGhNWYXVsdFByb2Nlc3NvclJ1bGVzIr8BarwBClcKVTpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EKYQpfWl0KWzpZYmFja2VuZC1zaWduYXRvcnktMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EqU2JpdHNhZmUtYWRtaW46OjEyMjA5OTk1MzkzNGQ5ZmUxNjNmZWQwN2RkMzcxZmExMzk4MmIyYjMwNzQ5ZDZkZjU2ZWNkYmEzODVmOGM3OGE4NjdhMlliYWNrZW5kLXNpZ25hdG9yeS0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YTl7TANp/EgGAEIqCiYKJAgBEiBvGY3UmUCK4oP4XdA6mUK7vNQUtI/KXjSgwk0UK1icBBAe"
            ),
            disclose!(
                allocation_factory_cid,
                "CgMyLjEShwYKRQDVil8GHwhrPEtAW1fKCPTO/LfzonomCJvmOI62LuYZqMoREiAJkdVl2qBMppGQG8BCOKhlXiwGgDkTDkwAAn6sJnXZ9BIXdXRpbGl0eS1yZWdpc3RyeS1hcHAtdjAajQEKQDgyNzk4ZGYwMTgzMDE4NTI3MDRmMjEwYjk3YWRhYWJmNzZkM2VjZDM3ZDg4OWUxYmY5NmI1ZjMxYTIwZWVhMzQSB1V0aWxpdHkSCFJlZ2lzdHJ5EgNBcHASAlYwEgdTZXJ2aWNlEhFBbGxvY2F0aW9uRmFjdG9yeRoRQWxsb2NhdGlvbkZhY3RvcnkioQJqngIKVgpUOlJjYnRjLW5ldHdvcms6OjEyMjAyYTgzYzZmNDA4MjIxN2MxNzVlMjliYzUzZGE1ZjI3MDNiYTI2NzU3NzhhYjk5MjE3YTVhODgxYTk0OTIwM2ZmClYKVDpSY2J0Yy1uZXR3b3JrOjoxMjIwMmE4M2M2ZjQwODIyMTdjMTc1ZTI5YmM1M2RhNWYyNzAzYmEyNjc1Nzc4YWI5OTIxN2E1YTg4MWE5NDkyMDNmZgpsCmo6aGF1dGgwXzAwN2M2NWY4NTdmMWMzZDU5OWNiNmRmNzM3NzU6OjEyMjBkMmQ3MzJkMDQyYzI4MWNlZTgwZjQ4M2FiODBmM2NiYWE0NzgyODYwZWQ1ZjRkYzIyOGFiMDNkZWRkMmVlOGY5KlJjYnRjLW5ldHdvcms6OjEyMjAyYTgzYzZmNDA4MjIxN2MxNzVlMjliYzUzZGE1ZjI3MDNiYTI2NzU3NzhhYjk5MjE3YTVhODgxYTk0OTIwM2ZmMmhhdXRoMF8wMDdjNjVmODU3ZjFjM2Q1OTljYjZkZjczNzc1OjoxMjIwZDJkNzMyZDA0MmMyODFjZWU4MGY0ODNhYjgwZjNjYmFhNDc4Mjg2MGVkNWY0ZGMyMjhhYjAzZGVkZDJlZThmOTlubOVh5DkGAEIqCiYKJAgBEiCdDhxHJbSFz7Snbvg8xLkPDPvaP3wl+HzTfq2LxHAGmRAe"
            ),
            // This is for the FAR config
            disclose!(
                "009b9fcd0ec3e6340d7fd1d75c192f6d7056c237465d94995fd87b6b3bc9bd091bca12122029b8d78f42969f04b90508b8cb1574d7c7142d2027c39c65717ff37b5667ed6c".to_string(),
                "CgMyLjESywQKRQCbn80Ow+Y0DX/R11wZL21wVsI3Rl2UmV/Ye2s7yb0JG8oSEiApuNePQpafBLkFCLjLFXTXxxQtICfDnGVxf/N7VmftbBINc3BsaWNlLWFtdWxldBpkCkAzY2ExMzQzYWIyNmI0NTNkMzhjOGFkYjcwZGNhNWYxZWFkODQ0MGM0MmI1OWI2OGYwNzA3ODY5NTVjYmY5ZWMxEgZTcGxpY2USBkFtdWxldBoQRmVhdHVyZWRBcHBSaWdodCKxAWquAQpNCks6SURTTzo6MTIyMGJlNThjMjllNjVkZTQwYmYyNzNiZTFkYzJiMjY2ZDQzYTlhMDAyZWE1YjE4OTU1YWVlZjdhYWM4ODFiYjQ3MWEKXQpbOlliYWNrZW5kLXNpZ25hdG9yeS0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YSpJRFNPOjoxMjIwYmU1OGMyOWU2NWRlNDBiZjI3M2JlMWRjMmIyNjZkNDNhOWEwMDJlYTViMTg5NTVhZWVmN2FhYzg4MWJiNDcxYTJZYmFja2VuZC1zaWduYXRvcnktMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2E5StpTtehIBgBCKgomCiQIARIgte09TNtngfU2IfZ5PIabI5+a9HM9ZLsg728k5xjF0GMQHg=="
            ),
        ],
        _ => vec![],
    }
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

    // Build choice argument: ExpireConfirmation { confirmationCid : ContractId VaultGovernanceConfirmation }
    let choice_argument = Value {
        sum: Some(value::Sum::Record(Record {
            record_id: None,
            fields: vec![RecordField {
                label: "confirmationCid".to_string(),
                value: Some(Value {
                    sum: Some(value::Sum::ContractId(request.confirmation_cid.clone())),
                }),
            }],
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
