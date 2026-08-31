//! Read-only token-standard, registry, and network-proxy endpoints.
//!
//! Split out of `governance.rs` (#282): these handlers query
//! services, credentials, transfer instructions/factories, holdings,
//! instruments, generic contracts, package configuration, and the DSO
//! network/operator proxies. They share nothing with the propose -> confirm
//! -> execute governance flow beyond a few helpers (`get_party_token`,
//! `packages`) imported from that module.

use std::collections::HashSet;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use serde::Deserialize;

use super::governance::{get_party_token, packages};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    server::{
        AppState,
        middleware::require_admin,
        queries::{
            ContractQueryParams as QueryContractParams, get_credential_offers, get_credentials,
            get_holdings, get_instruments, get_open_burn_requests, get_open_mint_requests,
            get_open_transfer_instructions, get_provider_configurations, get_provider_services,
            get_registrar_service_requests, get_registrar_services, get_transfer_factories,
            get_user_services, query_contracts_by_template,
        },
        types::{
            BurnRequestsResponse, ContractQueryResponse, CredentialOffersResponse,
            CredentialsResponse, ErrorResponse, HoldingsResponse, InstrumentsResponse,
            MintRequestsResponse, NetworkInfo, OperatorInfo, ProviderConfigurationsResponse,
            ProviderServicesResponse, RegistrarServiceRequestsResponse, RegistrarServicesResponse,
            TransferFactoriesResponse, TransferFactoryInfo, TransferInstructionsResponse,
            TransferPreapprovalsResponse, UserServicesResponse,
        },
    },
};

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
    let packages = packages();

    match get_provider_services(&data.config, party_id, token, &packages).await {
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
    let packages = packages();

    match get_user_services(&data.config, party_id, token, &packages).await {
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
    let packages = packages();

    match get_credential_offers(&data.config, party_id, token, &packages).await {
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

/// Get `Credential` contracts visible to the party. The accept mint/burn
/// request forms list these so the issuer credentials backing the accept can
/// be picked instead of pasted in by hand.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Credentials", body = CredentialsResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/credentials")]
pub async fn get_credentials_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;

    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    match get_credentials(&data.config, party_id, token, &packages).await {
        Ok(credentials) => HttpResponse::Ok().json(CredentialsResponse { credentials }),
        Err(e) => {
            tracing::error!("Failed to fetch credentials: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch credentials: {e}"),
            })
        }
    }
}

/// Get `RegistrarServiceRequest` contracts visible to the party. The
/// OnboardRegistrar form lists these so the request backing the onboard can
/// be picked instead of pasted in by hand.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (
            status = 200,
            description = "Registrar service requests",
            body = RegistrarServiceRequestsResponse,
        ),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/registrar-service-requests")]
pub async fn get_registrar_service_requests_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;

    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    match get_registrar_service_requests(&data.config, party_id, token, &packages).await {
        Ok(registrar_service_requests) => {
            HttpResponse::Ok().json(RegistrarServiceRequestsResponse {
                registrar_service_requests,
            })
        }
        Err(e) => {
            tracing::error!("Failed to fetch registrar service requests: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch registrar service requests: {e}"),
            })
        }
    }
}

/// Get `ProviderConfiguration` contracts visible to the party. The
/// OnboardRegistrar form lists these so the configuration backing the
/// onboard can be picked instead of pasted in by hand.
#[utoipa::path(
    tag = "Services",
    params(GovernanceQuery),
    responses(
        (
            status = 200,
            description = "Provider configurations",
            body = ProviderConfigurationsResponse,
        ),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/provider-configurations")]
pub async fn get_provider_configurations_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;

    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    match get_provider_configurations(&data.config, party_id, token, &packages).await {
        Ok(provider_configurations) => HttpResponse::Ok().json(ProviderConfigurationsResponse {
            provider_configurations,
        }),
        Err(e) => {
            tracing::error!("Failed to fetch provider configurations: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch provider configurations: {e}"),
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
    let packages = packages();

    match get_registrar_services(&data.config, party_id, token, &packages).await {
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
        params: &QueryContractParams,
        label: &str,
    ) -> usize {
        match query_contracts_by_template(config, party, token, params).await {
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
        &cc_preapproval,
        "CC TransferPreapproval",
    )
    .await;
    let cc_pending = count(
        &data.config,
        party_id,
        token.clone(),
        &cc_proposal,
        "CC TransferPreapprovalProposal",
    )
    .await;
    let token_count = count(
        &data.config,
        party_id,
        token,
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

    match get_instruments(&data.config, party_id, token).await {
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
            if let Ok(holdings) = get_holdings(&data.config, party_id, token).await {
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

    match get_holdings(&data.config, party_id, token).await {
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

    let contract_params = QueryContractParams {
        package_id: query.package_id.clone(),
        module_name: query.module_name.clone(),
        entity_name: query.entity_name.clone(),
        use_interface_filter: query.interface,
        active_only: query.active_only,
    };

    match query_contracts_by_template(&data.config, party_id, token, &contract_params).await {
        Ok(contracts) => HttpResponse::Ok().json(ContractQueryResponse { contracts }),
        Err(e) => {
            tracing::error!("Failed to query contracts: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to query contracts: {e}"),
            })
        }
    }
}

/// Get the node's package configuration.
///
/// Node-wide, not per party: the handler takes no arguments and the package
/// ids are constants. The annotation used to declare `params(GovernanceQuery)`
/// and say "for a party", which put a `party_id` query parameter in the
/// generated OpenAPI spec that the handler never reads.
#[utoipa::path(
    tag = "Configuration",
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
