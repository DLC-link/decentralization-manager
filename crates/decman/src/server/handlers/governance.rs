use std::collections::HashSet;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
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
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, PackageConfig, default_package_config},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    server::{
        AppState, action_serializer,
        audit::{AuditEvent, AuditParams, spawn_audit_log},
        chain_audit,
        middleware::require_admin,
        queries::{
            ContractQueryParams as QueryContractParams, get_credential_offers,
            get_governance_confirmations, get_governance_state as query_governance_state,
            get_holdings, get_instruments, get_open_burn_requests, get_open_mint_requests,
            get_open_transfer_instructions, get_provider_services, get_registrar_services,
            get_transfer_factories, get_user_services, get_vaults, query_contracts_by_template,
            resolve_contract_package_ref, select_input_holdings,
        },
        transfer_context::{
            AcceptTransferContext, ProposeTransferArgs, fetch as fetch_accept_transfer_context,
            fetch_factory_for_propose, maybe_fetch_for_proposal, needs_registry_context,
            to_proto_disclosed_contracts,
        },
        types::{
            AuditLogEntry, AuditLogQuery, AuditLogResponse, BurnRequestsResponse,
            CancelConfirmationRequest, ChainAuditEntry, ChainAuditQuery, ChainAuditResponse,
            ConfirmActionRequest, ContractQueryResponse, CredentialOffersResponse, ErrorResponse,
            ExecuteActionRequest, ExpireConfirmationRequest, GovernanceResponse,
            GovernanceStateResponse, GovernanceType, HoldingsResponse, InstrumentsResponse,
            KnownMember, KnownMembersResponse, MessageResponse, MintRequestsResponse, NetworkInfo,
            OperatorInfo, ProposalType, ProposeActionRequest, ProviderServicesResponse,
            RegistrarServicesResponse, TransferFactoriesResponse, TransferFactoryInfo,
            TransferInstructionsResponse, TransferPreapprovalsResponse, UserServicesResponse,
            VaultsResponse, chain_audit_entry_from_row,
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
    /// Drop contracts whose `executeBefore` deadline has already passed.
    /// Used by Accept Mint/Burn Request dropdowns so the user doesn't pick
    /// a contract that would fail at interpretation. No-op on templates
    /// without an `executeBefore` field.
    #[serde(default)]
    pub active_only: bool,
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

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;

    let member_party_id = get_member_party_id(&data, party_id).await;
    let packages = packages();

    // Pull `(rules_contract_id, threshold)` off the active GovernanceRules /
    // VaultGovernanceRules contract. The Daml `ExecuteGovernanceAction`
    // choice gates on THIS threshold ("Enough member confirmations to
    // execute action") — not the decentralized-namespace topology
    // threshold, which is a separate value used for signing
    // PartyToParticipant updates. Falling back to the DNS threshold for
    // historical compatibility only when the gov state isn't reachable.
    let (rules_contract_id, gov_state_threshold, gov_core_out_of_date, gov_core_package_ref) =
        match query_governance_state(&data.config, party_id, token.clone(), test_mode, &packages)
            .await
        {
            Ok(Some(state)) => (
                Some(state.contract_id),
                Some(state.threshold as usize),
                state.out_of_date,
                state.package_ref,
            ),
            Ok(None) => (None, None, false, None),
            Err(e) => {
                tracing::warn!("Failed to fetch active rules contract: {e}");
                (None, None, false, None)
            }
        };
    let threshold = match gov_state_threshold {
        Some(t) => t,
        None => get_party_threshold(&data, party_id).await.unwrap_or(2),
    };

    match get_governance_confirmations(
        &data.config,
        party_id,
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
            rules_contract_id,
            gov_core_out_of_date,
            gov_core_package_ref,
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

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match query_governance_state(&data.config, party_id, token, test_mode, &packages).await {
        Ok(state) => HttpResponse::Ok().json(GovernanceStateResponse { state }),
        Err(e) => {
            tracing::error!("Failed to fetch governance state: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch governance state: {e}"),
            })
        }
    }
}

#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Member parties known to each participant", body = KnownMembersResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/known-members")]
pub async fn get_known_members(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    match collect_known_members(&data, &query.party_id).await {
        Ok(members) => HttpResponse::Ok().json(KnownMembersResponse { members }),
        Err(e) => {
            tracing::error!("Failed to collect known members: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to collect known members: {e}"),
            })
        }
    }
}

async fn collect_known_members(
    data: &web::Data<AppState>,
    dec_party_id: &CantonId,
) -> Result<Vec<KnownMember>> {
    let dec_party_str = dec_party_id.to_string();
    let network_config = NetworkConfig::from_peers(data.db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&data.config.key_file_path()).await?;
    let self_id = data.config.participant_id();
    let identity_bytes = self_id.to_string();
    let request = Message::new(
        MessageType::RequestMemberParty,
        dec_party_str.as_bytes().to_vec(),
    );

    let mut out = Vec::new();

    {
        let creds = data.party_credentials.read().await;
        let self_member = creds
            .iter()
            .find(|c| c.dec_party_id == *dec_party_id)
            .map(|c| c.member_party_id.clone());
        out.push(KnownMember {
            participant_uid: self_id.clone(),
            member_party_id: self_member,
        });
    }

    for peer in &network_config.peers {
        if peer.participant_id == *self_id {
            continue;
        }
        if peer.public_key.is_empty() {
            out.push(KnownMember {
                participant_uid: peer.participant_id.clone(),
                member_party_id: None,
            });
            continue;
        }
        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping member-party query to {pid}: invalid public key: {e}",
                    pid = peer.participant_id,
                );
                out.push(KnownMember {
                    participant_uid: peer.participant_id.clone(),
                    member_party_id: None,
                });
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let response = match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity_bytes.as_bytes(),
            &request,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Failed to query member party from {pid}: {e}",
                    pid = peer.participant_id,
                );
                out.push(KnownMember {
                    participant_uid: peer.participant_id.clone(),
                    member_party_id: None,
                });
                continue;
            }
        };

        let member_party = match Message::from_bytes(&response) {
            Ok(msg) if msg.msg_type == MessageType::MemberPartyResponse => {
                String::from_utf8(msg.payload)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .and_then(|s| CantonId::parse(&s).ok())
            }
            _ => None,
        };
        out.push(KnownMember {
            participant_uid: peer.participant_id.clone(),
            member_party_id: member_party,
        });
    }

    Ok(out)
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

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_vaults(&data.config, party_id, token, test_mode, &packages).await {
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

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_provider_services(&data.config, party_id, token, test_mode, &packages).await {
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

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_user_services(&data.config, party_id, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(UserServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch user services: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch user services: {e}"),
            })
        }
    }
}

/// Get CredentialOffer contracts visible to the party
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Credential offers", body = CredentialOffersResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/credential-offers")]
pub async fn get_credential_offers_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_credential_offers(&data.config, party_id, token, test_mode, &packages).await {
        Ok(credential_offers) => {
            HttpResponse::Ok().json(CredentialOffersResponse { credential_offers })
        }
        Err(e) => {
            tracing::error!("Failed to fetch credential offers: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch credential offers: {e}"),
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

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;
    let packages = packages();

    match get_registrar_services(&data.config, party_id, token, test_mode, &packages).await {
        Ok(services) => HttpResponse::Ok().json(RegistrarServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch registrar services: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch registrar services: {e}"),
            })
        }
    }
}

/// List open `TransferInstruction` contracts (status
/// `TransferPendingReceiverAcceptance`) addressed to this dec-party. Used by
/// the Accept Transfer proposal form to populate a dropdown of acceptable
/// transfers — operators pick from this list instead of pasting the contract
/// id.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (
            status = 200,
            description = "Open transfer instructions",
            body = TransferInstructionsResponse,
        ),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    )
)]
#[get("/governance/transfer-instructions")]
pub async fn get_transfer_instructions_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let token = get_party_token(&data, party_id).await;

    match get_open_transfer_instructions(&data.config, party_id, token).await {
        Ok(transfer_instructions) => HttpResponse::Ok().json(TransferInstructionsResponse {
            transfer_instructions,
        }),
        Err(e) => {
            tracing::error!("Failed to fetch transfer instructions: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch transfer instructions: {e}"),
            })
        }
    }
}

/// Open `MintRequest` contracts the governance party can accept. Returns
/// typed fields (holder, amount, instrument) so the Accept Mint Request
/// dropdown can surface a human-readable label instead of just the cid.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Open mint requests", body = MintRequestsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    )
)]
#[get("/governance/mint-requests")]
pub async fn get_mint_requests_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    match get_open_mint_requests(&data.config, party_id, token, &packages).await {
        Ok(mint_requests) => HttpResponse::Ok().json(MintRequestsResponse { mint_requests }),
        Err(e) => {
            tracing::error!("Failed to fetch mint requests: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch mint requests: {e}"),
            })
        }
    }
}

/// Open `BurnRequest` contracts. Mirrors `/governance/mint-requests`.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Open burn requests", body = BurnRequestsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    )
)]
#[get("/governance/burn-requests")]
pub async fn get_burn_requests_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    match get_open_burn_requests(&data.config, party_id, token, &packages).await {
        Ok(burn_requests) => HttpResponse::Ok().json(BurnRequestsResponse { burn_requests }),
        Err(e) => {
            tracing::error!("Failed to fetch burn requests: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch burn requests: {e}"),
            })
        }
    }
}

/// Count active `TransferPreapproval` contracts visible to this party, split
/// between Canton Coin (Splice.Wallet) and utility-token (Utility.Registry)
/// variants. Used by the proposal forms to warn that re-issuing a CC / Token
/// preapproval would be a no-op when one already exists.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Preapproval counts", body = TransferPreapprovalsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/transfer-preapprovals")]
pub async fn get_transfer_preapprovals_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;

    // Canton Coin: the actual `TransferPreapproval` template lives in
    // `Splice.AmuletRules` (signatories: receiver, provider, dso — gov party
    // sees it as receiver). The intermediate `TransferPreapprovalProposal`
    // (in `Splice.Wallet.TransferPreapproval`) is what the gov flow creates
    // right after execution and sits there until the DSO accepts it; we
    // count both so the warning fires regardless of which stage you're in.
    let cc_preapproval = QueryContractParams {
        package_id: "#splice-amulet".to_string(),
        module_name: "Splice.AmuletRules".to_string(),
        entity_name: "TransferPreapproval".to_string(),
        use_interface_filter: false,
        active_only: false,
    };
    let cc_proposal = QueryContractParams {
        package_id: "#splice-amulet".to_string(),
        module_name: "Splice.Wallet.TransferPreapproval".to_string(),
        entity_name: "TransferPreapprovalProposal".to_string(),
        use_interface_filter: false,
        active_only: false,
    };
    let token_params = QueryContractParams {
        package_id: "#utility-registry-app-v0".to_string(),
        module_name: "Utility.Registry.App.V0.Model.TransferPreapproval".to_string(),
        entity_name: "TransferPreapproval".to_string(),
        use_interface_filter: false,
        active_only: false,
    };

    async fn count(
        config: &crate::config::NodeConfig,
        party: &CantonId,
        token: Option<String>,
        test_mode: bool,
        params: &QueryContractParams,
        label: &str,
    ) -> usize {
        match query_contracts_by_template(config, party, token, test_mode, params).await {
            Ok(c) => c.len(),
            Err(e) => {
                // Template-not-uploaded means there are simply no such
                // contracts on this participant — a legitimate 0, not a
                // failure worth a WARN.
                if e.to_string()
                    .contains("NO_TEMPLATES_FOR_PACKAGE_NAME_AND_QUALIFIED_NAME")
                {
                    tracing::debug!(
                        "No {label} templates uploaded on this participant; counting as 0",
                    );
                } else {
                    tracing::warn!("Failed to query {label}: {e}");
                }
                0
            }
        }
    }

    let cc_accepted = count(
        &data.config,
        party_id,
        token.clone(),
        test_mode,
        &cc_preapproval,
        "CC TransferPreapproval",
    )
    .await;
    let cc_pending = count(
        &data.config,
        party_id,
        token.clone(),
        test_mode,
        &cc_proposal,
        "CC TransferPreapprovalProposal",
    )
    .await;
    let token_count = count(
        &data.config,
        party_id,
        token,
        test_mode,
        &token_params,
        "utility TransferPreapproval",
    )
    .await;

    HttpResponse::Ok().json(TransferPreapprovalsResponse {
        cc: cc_accepted + cc_pending,
        token: token_count,
    })
}

/// Get InstrumentConfiguration contracts for a party. Each one represents a
/// token the governance party can mint/burn against; the response includes the
/// `instrument_admin` and `instrument_id` parsed from the contract's
/// `defaultIdentifier` so the frontend can populate Mint/Burn forms without
/// reading the contract blob.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Available instruments", body = InstrumentsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/instruments")]
pub async fn get_instruments_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;

    match get_instruments(&data.config, party_id, token, test_mode).await {
        Ok(instruments) => HttpResponse::Ok().json(InstrumentsResponse { instruments }),
        Err(e) => {
            tracing::error!("Failed to fetch instruments: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch instruments: {e}"),
            })
        }
    }
}

/// List active `TransferFactory` contracts visible to the party. Used by the
/// Transfer Proposal form to prefill the factory contract id and expected
/// admin once the user picks an instrument from the dropdown.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Transfer factories", body = TransferFactoriesResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/transfer-factories")]
pub async fn get_transfer_factories_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let token = get_party_token(&data, party_id).await;

    match get_transfer_factories(&data.config, party_id, token.clone()).await {
        Ok(mut transfer_factories) => {
            // Canton Coin's TransferFactory implementation is the system
            // `Splice.AmuletRules:AmuletRules` contract, which the ledger
            // interface query above doesn't surface to feature parties. The
            // DSO API publishes its contract id; expose it as a synthetic
            // factory keyed on the DSO party so the Transfer Proposal form's
            // existing `expected_admin == holding.instrument_admin` join
            // matches CC holdings (whose instrument_admin is the DSO).
            if let Some((dso_party_id, amulet_rules_cid)) =
                fetch_amulet_rules_factory(&data.http_client, &data.config).await
            {
                transfer_factories.push(TransferFactoryInfo {
                    contract_id: amulet_rules_cid,
                    expected_admin: dso_party_id,
                });
            }
            // Shared-instrument tokens (e.g. CBTC, admin = `cbtc-network`)
            // don't expose a `TransferFactory` on this dec party's ACS —
            // the factory lives on the registrar. Surface a placeholder
            // entry per unique non-self admin so the dropdown enables the
            // holding; the propose handler resolves the real factory cid +
            // choice context from the registrar at submit time.
            let mut existing_admins: HashSet<String> = transfer_factories
                .iter()
                .map(|f| f.expected_admin.to_string())
                .collect();
            existing_admins.insert(party_id.to_string());
            if let Ok(holdings) = get_holdings(&data.config, party_id, token, data.test_mode).await
            {
                for holding in holdings {
                    let admin_str = holding.instrument_admin.to_string();
                    if existing_admins.insert(admin_str) {
                        transfer_factories.push(TransferFactoryInfo {
                            contract_id: String::new(),
                            expected_admin: holding.instrument_admin,
                        });
                    }
                }
            }
            HttpResponse::Ok().json(TransferFactoriesResponse { transfer_factories })
        }
        Err(e) => {
            tracing::error!("Failed to fetch transfer factories: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch transfer factories: {e}"),
            })
        }
    }
}

/// Pull the DSO party id and AmuletRules contract id from the DSO API. Returns
/// `None` (with a logged warning) on any failure so callers can degrade
/// gracefully — the only consumer is `/transfer-factories`, which omits CC
/// rather than failing the whole response when the DSO API is unreachable.
async fn fetch_amulet_rules_factory(
    http_client: &reqwest::Client,
    config: &NodeConfig,
) -> Option<(CantonId, String)> {
    let url = config.canton.network.dso_url();
    let res = match http_client.get(url).send().await {
        Ok(res) if res.status().is_success() => res,
        Ok(res) => {
            tracing::warn!("DSO API returned {} fetching AmuletRules", res.status());
            return None;
        }
        Err(e) => {
            tracing::warn!("Failed to reach DSO API for AmuletRules: {e}");
            return None;
        }
    };
    let json: serde_json::Value = res
        .json()
        .await
        .inspect_err(|e| tracing::warn!("Failed to parse DSO response: {e}"))
        .ok()?;
    let dso = json.pointer("/dso_party_id").and_then(|v| v.as_str())?;
    let cid = json
        .pointer("/amulet_rules/contract/contract_id")
        .and_then(|v| v.as_str())?;
    Some((dso.parse().ok()?, cid.to_string()))
}

/// Get token-standard `Holding` contracts owned by a party, aggregated by
/// `(instrument_admin, instrument_id)`. Each row also reports whether a
/// `TransferPreapproval` is in place for that instrument so the frontend can
/// render a Yes/No badge without a second round-trip.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Party holdings", body = HoldingsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/holdings")]
pub async fn get_holdings_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;

    match get_holdings(&data.config, party_id, token, test_mode).await {
        Ok(holdings) => HttpResponse::Ok().json(HoldingsResponse { holdings }),
        Err(e) => {
            tracing::error!("Failed to fetch holdings: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch holdings: {e}"),
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

    let token = get_party_token(&data, party_id).await;
    let test_mode = data.test_mode;

    let contract_params = QueryContractParams {
        package_id: query.package_id.clone(),
        module_name: query.module_name.clone(),
        entity_name: query.entity_name.clone(),
        use_interface_filter: query.interface,
        active_only: query.active_only,
    };

    match query_contracts_by_template(&data.config, party_id, token, test_mode, &contract_params)
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
                .filter_map(|row| {
                    // Skip entries with malformed party ids — they're DB-level
                    // corruption, not something the API should propagate.
                    let party_id = CantonId::parse(&row.party_id)
                        .map_err(|e| {
                            tracing::warn!(
                                "Skipping audit row {id}: bad party_id '{p}': {e}",
                                id = row.id,
                                p = row.party_id
                            );
                        })
                        .ok()?;
                    let member_party_id = CantonId::parse(&row.member_party_id)
                        .map_err(|e| {
                            tracing::warn!(
                                "Skipping audit row {id}: bad member_party_id '{m}': {e}",
                                id = row.id,
                                m = row.member_party_id
                            );
                        })
                        .ok()?;
                    Some(AuditLogEntry {
                        id: row.id,
                        timestamp: row.timestamp,
                        event_type: row.event_type,
                        party_id,
                        member_party_id,
                        governance_type: row.governance_type,
                        action_summary: row.action_summary,
                        details: serde_json::from_str(&row.details)
                            .unwrap_or(serde_json::Value::String(row.details)),
                        status: row.status,
                        error_message: row.error_message,
                        created_at: row.created_at,
                    })
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

    if !query.refresh {
        // Return from cache
        match data
            .db
            .get_chain_audit_cache(party_id, query.limit as i64)
            .await
        {
            Ok(rows) => {
                let entries: Vec<ChainAuditEntry> =
                    rows.into_iter().map(chain_audit_entry_from_row).collect();
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

    match chain_audit::get_chain_audit(&data.config, party_id, token, &pkgs, query.limit).await {
        Ok(entries) => {
            // Save to cache in background
            let pool = data.db.clone();
            let pid = party_id.clone();
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
            tracing::error!("Failed to fetch chain audit for {party_id}: {e:#}");
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
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/propose")]
pub async fn propose_action(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ProposeActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if let Err(msg) = body.proposal.validate() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: msg.to_string(),
        });
    }
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
    let audit_party_id = party_id.clone();
    let audit_member = member_party_id.clone();

    let packages = packages();

    // Resolve registry-backed context for token-standard transfer flows:
    //   * `AcceptTransfer`: fetch the `transfer-rule` choice context the
    //     `TransferInstruction_Accept` choice reads at execute time. Without it
    //     execute fails with `Missing context entry for
    //     utility.digitalasset.com/transfer-rule`.
    //   * `Transfer` of a utility-registry instrument: `TransferFactory_Transfer`
    //     reads `utility.digitalasset.com/instrument-configuration` from
    //     `extraArgs.context.values` at execute time, so the context must be
    //     fetched from the registrar and baked into the proposal regardless of
    //     whether the dec party administers the instrument. For shared
    //     instruments (e.g. CBTC, admin = `cbtc-network`) the factory isn't on
    //     the dec party's ACS, so we also substitute the resolved factory cid.
    //     Canton Coin is excluded — its `AmuletRules` factory and context come
    //     from the DSO scan API. See `needs_registry_context`.
    // Bounded validity window for any Transfer proposal, captured ONCE so the
    // registry choice-context fetch and the on-chain create args agree
    // byte-for-byte. A bounded `executeBefore` lets an unaccepted two-step offer
    // expire and release its escrow instead of locking funds forever. The
    // window defaults to 24h but the caller may override it per-transfer.
    let now_micros = chrono::Utc::now().timestamp_micros();
    let transfer_validity = match &body.proposal {
        ProposalType::Transfer {
            validity_window_hours: Some(hours),
            ..
        } => action_serializer::TransferValidity::from_now_with_window(
            now_micros,
            i64::from(*hours).saturating_mul(60 * 60 * 1_000_000),
        ),
        _ => action_serializer::TransferValidity::from_now(now_micros),
    };

    let mut resolved_proposal = body.proposal.clone();
    let transfer_choice_context = match &mut resolved_proposal {
        ProposalType::AcceptTransfer {
            transfer_instruction_cid,
        } => match fetch_accept_transfer_context(
            &data.config,
            Some(token.clone()),
            data.config.canton.network,
            party_id,
            transfer_instruction_cid,
        )
        .await
        {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch AcceptTransfer choice context from registry: {e:#}"
                );
                return HttpResponse::BadGateway().json(ErrorResponse {
                    error: format!("Failed to fetch transfer choice context: {e}"),
                });
            }
        },
        ProposalType::Transfer {
            transfer_factory_cid,
            receiver,
            amount,
            instrument_id,
            input_holding_cids,
            ..
        } if needs_registry_context(
            transfer_factory_cid,
            &instrument_id.admin,
            &party_id.to_string(),
        ) =>
        {
            let admin: CantonId = match instrument_id.admin.parse() {
                Ok(p) => p,
                Err(e) => {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: format!("Invalid instrument admin party id: {e}"),
                    });
                }
            };
            // The token-standard transfer factory rejects an empty
            // `inputHoldingCids` ("No holdings provided"). When the caller
            // didn't pin specific holdings, fund the transfer with every
            // Holding the sender owns for this instrument and let the choice
            // consume what it needs (returning change).
            if input_holding_cids.is_empty() {
                match select_input_holdings(
                    &data.config,
                    party_id,
                    Some(token.clone()),
                    &admin,
                    &instrument_id.id,
                )
                .await
                {
                    Ok(cids) if cids.is_empty() => {
                        return HttpResponse::BadRequest().json(ErrorResponse {
                            error: format!(
                                "No holdings of instrument {} owned by {} to fund the transfer",
                                instrument_id.id, party_id
                            ),
                        });
                    }
                    Ok(cids) => *input_holding_cids = cids,
                    Err(e) => {
                        tracing::warn!("Failed to select input holdings for transfer: {e:#}");
                        return HttpResponse::InternalServerError().json(ErrorResponse {
                            error: format!("Failed to select input holdings: {e}"),
                        });
                    }
                }
            }
            match fetch_factory_for_propose(
                data.config.canton.network,
                ProposeTransferArgs {
                    sender: party_id,
                    receiver,
                    amount,
                    instrument_admin: &admin,
                    instrument_id: &instrument_id.id,
                    input_holding_cids,
                    requested_at_micros: transfer_validity.requested_at_micros,
                    execute_before_micros: transfer_validity.execute_before_micros,
                },
            )
            .await
            {
                Ok(resolved) => {
                    // Self-administered utility tokens already carry the factory
                    // cid the UI read from the dec party's ACS; only fill it in
                    // for shared instruments where the UI left it empty.
                    if transfer_factory_cid.is_empty() {
                        *transfer_factory_cid = resolved.factory_cid;
                    }
                    Some(AcceptTransferContext {
                        context: resolved.context,
                        disclosed_contracts: resolved.disclosed_contracts,
                    })
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch Transfer choice context from registry: {e:#}");
                    return HttpResponse::BadGateway().json(ErrorResponse {
                        error: format!("Failed to fetch transfer factory: {e}"),
                    });
                }
            }
        }
        _ => None,
    };

    let (package_source, module_name, entity_name, create_args) =
        match action_serializer::build_proposal_create_args(
            &party_id.to_string(),
            &member_party_id.to_string(),
            &resolved_proposal,
            transfer_choice_context.as_ref().map(|r| &r.context),
            Some(transfer_validity),
        ) {
            Ok(args) => args,
            Err(e) => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!("Failed to build proposal create arguments: {e}"),
                });
            }
        };

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
        action_serializer::ProposalPackage::GovernanceUtilityCredential => {
            match packages.governance_utility_credential.as_deref() {
                Some(pkg) => pkg,
                None => {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: "governance_utility_credential package not configured".to_string(),
                    });
                }
            }
        }
        action_serializer::ProposalPackage::GovernanceUtilityOnboarding => {
            match packages.governance_utility_onboarding.as_deref() {
                Some(pkg) => pkg,
                None => {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: "governance_utility_onboarding package not configured".to_string(),
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
        act_as: vec![member_party_id.to_string()],
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

    // The rules contract may be an out-of-date fallback living under an older
    // governance-core package — exercise it under its actual package ref.
    let rules_package_ref = resolve_contract_package_ref(
        &data.config,
        party_id,
        Some(token.clone()),
        &body.rules_contract_id,
        governance_core_pkg,
    )
    .await;

    let confirm_template = Identifier {
        package_id: rules_package_ref,
        module_name: "Governance.Rules".to_string(),
        entity_name: "GovernanceRules".to_string(),
    };

    let confirm_arg = action_serializer::build_confirm_domain_action_arg(
        &member_party_id.to_string(),
        &proposal_cid,
    );

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
        act_as: vec![member_party_id.to_string()],
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
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/confirm")]
pub async fn confirm_action(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ConfirmActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
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
    let audit_party_id = party_id.clone();
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
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/execute")]
pub async fn execute_action(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ExecuteActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
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
    let audit_party_id = party_id.clone();
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
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/expire")]
pub async fn expire_confirmation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ExpireConfirmationRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
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
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.clone();
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
    let audit_party_id = party_id.clone();
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

    match data.http_client.get(url).send().await {
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

/// Get DA Utility operator party ID
#[utoipa::path(
    tag = "Proxy",
    responses(
        (status = 200, description = "Operator info", body = OperatorInfo),
        (status = 502, description = "Operator API error", body = ErrorResponse)
    )
)]
#[get("/operator-info")]
pub async fn get_operator_info(data: web::Data<AppState>) -> impl Responder {
    let url = data.config.canton.network.operator_url();

    match data.http_client.get(url).send().await {
        Ok(res) if res.status().is_success() => match res.json::<serde_json::Value>().await {
            Ok(json) => match json.pointer("/partyId").and_then(|v| v.as_str()) {
                Some(party) => match party.parse::<CantonId>() {
                    Ok(party_id) => HttpResponse::Ok().json(OperatorInfo { party_id }),
                    Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
                        error: format!("Invalid operator party ID: {e}"),
                    }),
                },
                None => {
                    tracing::warn!("Unexpected operator API response format");
                    HttpResponse::BadGateway().json(ErrorResponse {
                        error: "Unexpected response format from operator API".to_string(),
                    })
                }
            },
            Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Failed to parse operator response: {e}"),
            }),
        },
        Ok(res) => {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            tracing::error!("Operator API returned {status}: {body}");
            HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Operator API returned {status}: {body}"),
            })
        }
        Err(e) => HttpResponse::BadGateway().json(ErrorResponse {
            error: format!("Failed to reach operator API: {e}"),
        }),
    }
}

/// Proxy request to fetch token standard contracts (avoids CORS)
#[utoipa::path(
    tag = "Proxy",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Token standard contracts"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 502, description = "Bad gateway", body = ErrorResponse)
    )
)]
#[post("/token-standard-contracts")]
pub async fn get_token_standard_contracts(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let url = "https://devnet.dlc.link/peer-2/app/get-token-standard-contracts";

    match data
        .http_client
        .post(url)
        .json(&body.into_inner())
        .send()
        .await
    {
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
async fn get_party_threshold(data: &web::Data<AppState>, party_id: &CantonId) -> Option<usize> {
    let namespace = party_id.namespace.to_hex();

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
                filter_namespace: namespace,
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
async fn get_member_party_id(data: &web::Data<AppState>, party_id: &CantonId) -> Option<CantonId> {
    let party_creds = data.party_credentials.read().await;
    party_creds
        .iter()
        .find(|p| &p.dec_party_id == party_id)
        .map(|p| p.member_party_id.clone())
}

/// Get token and member_party_id for a party
async fn get_party_credentials(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<(String, CantonId)> {
    let auth = data.auth.read().await;
    match &*auth {
        Some(WorkflowAuth::Keycloak(registry)) => {
            let tm = registry.get(party_id)?;
            let token = tm.get_token().await.ok()?;
            Some((token, tm.member_party_id().clone()))
        }
        Some(WorkflowAuth::Mock(mock_registry)) => {
            let mm = mock_registry.get(party_id).await;
            Some((mm.get_token(), mm.member_party_id().clone()))
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
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    let member_party_id_str = member_party_id.to_string();
    let member_party_id = member_party_id_str.as_str();
    let (mut template_id, choice, choice_argument) = match request.governance_type {
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

    // The rules contract may be an out-of-date fallback living under an older
    // governance-core package — exercise it under its actual package ref.
    if matches!(
        request.governance_type,
        GovernanceType::CoreSelf | GovernanceType::CoreDomain
    ) {
        template_id.package_id = resolve_contract_package_ref(
            config,
            &request.party_id,
            Some(token.to_string()),
            &request.rules_contract_id,
            &template_id.package_id,
        )
        .await;
    }

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
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    let member_party_id_str = member_party_id.to_string();
    let member_party_id = member_party_id_str.as_str();
    let (mut template_id, choice, choice_argument) = match request.governance_type {
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

    // The rules contract may be an out-of-date fallback living under an older
    // governance-core package — exercise it under its actual package ref.
    if matches!(
        request.governance_type,
        GovernanceType::CoreSelf | GovernanceType::CoreDomain
    ) {
        template_id.package_id = resolve_contract_package_ref(
            config,
            &request.party_id,
            Some(token.to_string()),
            &request.rules_contract_id,
            &template_id.package_id,
        )
        .await;
    }

    // For `AcceptTransferProposal` execution the executor's submission must
    // include the registry-supplied disclosed contracts (transfer rule + its
    // dependencies). `maybe_fetch_for_proposal` template-id-checks the
    // on-chain proposal and returns `Ok(None)` for anything else, so we don't
    // gate on `governance_type` here — that would silently drop the fetch on
    // any non-CoreDomain path that happens to carry an AcceptTransferProposal.
    let mut registry_disclosed: Vec<DisclosedContract> = Vec::new();
    if let Some(proposal_cid) = request.proposal_cid.as_deref() {
        match maybe_fetch_for_proposal(
            config,
            Some(token.to_string()),
            &request.party_id,
            proposal_cid,
        )
        .await
        {
            Ok(Some(ctx)) => match to_proto_disclosed_contracts(&ctx.disclosed_contracts) {
                Ok(dcs) => registry_disclosed = dcs,
                Err(e) => {
                    // A malformed blob from the registry shouldn't 500 the
                    // execute call — mirror the non-fatal handling below.
                    tracing::warn!(
                        "Failed to decode registry disclosed contracts for proposal {proposal_cid}: {e:#}"
                    );
                }
            },
            Ok(None) => {}
            Err(e) => {
                // Don't hard-fail on registry hiccups for non-transfer
                // proposals; for transfer proposals the Daml choice will
                // surface a clear `Missing context entry` error.
                tracing::warn!(
                    "Failed to fetch transfer choice context for proposal {proposal_cid}: {e:#}"
                );
            }
        }
    }

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

    let mut disclosed_contracts: Vec<DisclosedContract> = request
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
        .collect::<Result<Vec<_>>>()?;
    // De-dup by contract id — the FE may forward something the registry also
    // returned; keep the first occurrence (FE-supplied).
    let seen: HashSet<String> = disclosed_contracts
        .iter()
        .map(|d| d.contract_id.clone())
        .collect();
    disclosed_contracts.extend(
        registry_disclosed
            .into_iter()
            .filter(|d| !seen.contains(&d.contract_id)),
    );

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
        disclosed_contracts,
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
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    let member_party_id_str = member_party_id.to_string();
    let member_party_id = member_party_id_str.as_str();
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

    let (mut template_id, choice) = match request.governance_type {
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
                "GovernanceRules_ExpireGovernanceSelfConfirmation".to_string(),
            )
        }
        GovernanceType::CoreDomain => {
            // Same `GovernanceRules` template as CoreSelf but a different choice:
            // `GovernanceRules_ExpireConfirmation` operates on the
            // `GovernanceConfirmation` template (domain action confirmations)
            // rather than `GovernanceSelfConfirmation`. Same argument shape
            // ({ member, staleConfirmationCid }) so the choice_argument above
            // is reused as-is.
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
                "GovernanceRules_ExpireConfirmation".to_string(),
            )
        }
    };

    // The rules contract may be an out-of-date fallback living under an older
    // governance-core package — exercise it under its actual package ref.
    if matches!(
        request.governance_type,
        GovernanceType::CoreSelf | GovernanceType::CoreDomain
    ) {
        template_id.package_id = resolve_contract_package_ref(
            config,
            &request.party_id,
            Some(token.to_string()),
            &request.rules_contract_id,
            &template_id.package_id,
        )
        .await;
    }

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
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    let member_party_id_str = member_party_id.to_string();
    let member_party_id = member_party_id_str.as_str();
    let (mut template_id, choice) = match request.governance_type {
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
            // Domain confirmations live in their own template
            // `GovernanceConfirmation` (module `Governance.Confirmation`).
            // The `Cancel` choice is controller=confirmer with no arguments.
            let pkg = packages
                .governance_core
                .as_deref()
                .context("governance_core package not configured")?;
            (
                Identifier {
                    package_id: pkg.to_string(),
                    module_name: "Governance.Confirmation".to_string(),
                    entity_name: "GovernanceConfirmation".to_string(),
                },
                "GovernanceConfirmation_Cancel".to_string(),
            )
        }
    };

    // The confirmation contract is created by the rules contract's choice, so
    // it shares the rules contract's (possibly out-of-date) package —
    // exercise it under its actual package ref.
    if matches!(
        request.governance_type,
        GovernanceType::CoreSelf | GovernanceType::CoreDomain
    ) {
        template_id.package_id = resolve_contract_package_ref(
            config,
            &request.party_id,
            Some(token.to_string()),
            &request.confirmation_cid,
            &template_id.package_id,
        )
        .await;
    }

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
