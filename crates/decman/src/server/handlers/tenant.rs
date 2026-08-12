//! Wallet-facing tenant API (`/v0/tenant/*`).
//!
//! These endpoints let a wallet provider drive external-party onboarding and
//! transacting on behalf of a party whose Ed25519 namespace key the wallet
//! generates and holds — DPM only relays. Every binary field on the wire is
//! base64 (STANDARD engine).
//!
//! Unlike the admin endpoints (Keycloak JWT + `require_admin`), tenant callers
//! authenticate with a provider-issued API key, so every handler here calls
//! [`require_tenant_api_key`] first (the `/v0/tenant/` prefix bypasses the JWT
//! middleware).
//!
//! JSON ↔ Daml `Value` mapping (prepare-submission / ACS) is best-effort: with
//! no Daml type schema on hand a JSON string that parses as a Canton party id is
//! treated as a `Party` and everything else as `Text`, JSON integers become
//! `Int64` and other numbers `Numeric`, objects become records, and arrays
//! become lists. Variants, enums, maps, dates, and contract-id-typed fields are
//! not distinguishable from the plain JSON and are not produced on the way in.

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};
use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, CreateCommand, DisclosedContract, ExerciseCommand, GetActiveContractsRequest,
    GetLedgerEndRequest, Identifier, List, Optional, Record, RecordField, Signature,
    SignatureFormat, SigningAlgorithmSpec, Value, command,
    get_active_contracts_response::ContractEntry,
    interactive::{
        ExecuteSubmissionRequest, PartySignatures, PrepareSubmissionRequest, PreparedTransaction,
        SinglePartySignatures,
    },
    value,
};
use prost::Message as _;
use uuid::Uuid;

use crate::{
    canton_id::{CantonId, validate_party_id_prefix},
    config::NodeConfig,
    error::Result,
    server::{
        AppState, WorkflowAuth,
        action_serializer::TransferValidity,
        event_filters::{party_event_format, wildcard_filter},
        middleware::require_tenant_api_key,
        queries, token_standard,
        transfer_context::to_proto_disclosed_contracts,
        types::{
            ErrorResponse, TenantAcceptTransferRequest, TenantAcsResponse, TenantCommand,
            TenantContract, TenantDisclosedContract, TenantExecuteSubmissionRequest, TenantHolding,
            TenantHoldingsResponse, TenantOnboardRequest, TenantOnboardResponse,
            TenantPrepareRequest, TenantPrepareResponse, TenantPrepareSubmissionRequest,
            TenantPrepareSubmissionResponse, TenantTemplateId, TenantTransferOffer,
            TenantTransferOffersResponse, TenantTransferRequest, TransferInstructionInfo,
            TransferInstructionStatus, WorkflowProgress, WorkflowStatusResponse,
        },
    },
    utils,
    workflow::external_party::{
        keys::fingerprint_from_public_key,
        steps::{
            ExternalPartyAllocatePayload, HostOnboardingStatus, allocate_party,
            host_onboarding_status, prepare_topology,
        },
    },
};

use super::workflows::validate_confirmation_threshold;

// ============================================================================
// Onboarding (wallet-driven)
// ============================================================================

/// Prepare the onboarding topology for a wallet-held external party: DPM relays
/// the wallet's public key to Canton and returns the unsigned topology + the
/// per-transaction hashes for the wallet to sign.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantPrepareRequest,
    responses(
        (status = 200, description = "Unsigned onboarding topology", body = TenantPrepareResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/prepare")]
pub async fn tenant_prepare(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantPrepareRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    if let Err(msg) = validate_party_id_prefix(&body.party_hint) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }
    if body.hosting_peers.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "hosting_peers must name at least one other participant to host the party"
                .to_string(),
        });
    }
    let num_hosts = body.hosting_peers.len() + 1;
    if let Err(msg) = validate_confirmation_threshold(body.confirmation_threshold, num_hosts) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }

    let public_key = match decode_public_key(&body.public_key) {
        Ok(pk) => pk,
        Err(resp) => return resp,
    };

    // No run is registered here — the wallet signs the returned hashes and calls
    // `/v0/tenant/onboard` next.
    match prepare_topology(
        &data.config,
        &body.party_hint,
        &body.hosting_peers,
        body.confirmation_threshold,
        &public_key,
    )
    .await
    {
        Ok(prep) => HttpResponse::Ok().json(TenantPrepareResponse {
            party_id: prep.party_id,
            transaction_hashes: prep
                .transaction_hashes
                .iter()
                .map(|h| STANDARD.encode(h))
                .collect(),
            topology_transactions: prep
                .topology_transactions
                .iter()
                .map(|tx| STANDARD.encode(tx))
                .collect(),
        }),
        Err(e) => {
            tracing::error!("tenant prepare: topology generation failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to prepare onboarding topology: {e}"),
            })
        }
    }
}

/// Onboard a wallet-held external party from its signed topology on THIS host:
/// DPM allocates the wallet-signed bundle on its own participant only. The wallet
/// calls this on every host itself; no host relays to another. Idempotent
/// (`ALREADY_EXISTS` counts as success), so the wallet can safely retry a host.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantOnboardRequest,
    responses(
        (status = 202, description = "Allocated on this host (status reflects this host's view)", body = TenantOnboardResponse),
        (status = 400, description = "Bad request (bad base64, or signed_by != public_key fingerprint)", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "AllocateExternalParty failed on this participant", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/onboard")]
pub async fn tenant_onboard(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantOnboardRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    if let Err(msg) = validate_party_id_prefix(&body.party_hint) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }

    // The party id is built from the client-supplied `signed_by`, so it must be
    // the real fingerprint of `public_key`; otherwise a mismatched pair would
    // have us record and return a party id that doesn't match the key the
    // topology was signed with. Fail fast with a 400.
    let public_key = match decode_public_key(&body.public_key) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let derived_fingerprint = fingerprint_from_public_key(&public_key);
    if derived_fingerprint != body.signed_by {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "signed_by ({signed_by}) does not match the fingerprint derived from public_key \
                 ({derived_fingerprint})",
                signed_by = body.signed_by
            ),
        });
    }
    let mut signatures = Vec::with_capacity(body.signatures.len());
    for sig in &body.signatures {
        match STANDARD.decode(sig) {
            Ok(bytes) => signatures.push(bytes),
            Err(e) => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!("signature is not valid base64: {e}"),
                });
            }
        }
    }
    let mut topology_transactions = Vec::with_capacity(body.topology_transactions.len());
    for tx in &body.topology_transactions {
        match STANDARD.decode(tx) {
            Ok(bytes) => topology_transactions.push(bytes),
            Err(e) => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!("topology transaction is not valid base64: {e}"),
                });
            }
        }
    }

    if signatures.len() != topology_transactions.len() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "expected one signature per topology transaction, got {sigs} signature(s) for \
                 {txs} transaction(s)",
                sigs = signatures.len(),
                txs = topology_transactions.len()
            ),
        });
    }

    let party_id = format!("{hint}::{fp}", hint = body.party_hint, fp = body.signed_by);
    let bundle = ExternalPartyAllocatePayload {
        party_id: party_id.clone(),
        public_key,
        topology_transactions,
        signatures,
        signed_by: body.signed_by.clone(),
    };

    // Submit on THIS participant only. The wallet calls `/onboard` on every host
    // itself; no host relays to another. Canton keeps the topology a proposal until
    // every host has authorized it, and re-submitting an identical transaction is a
    // no-op, so a wallet retry converges.
    let ledger_token = node_ledger_token(&data).await;
    if let Err(e) = allocate_party(&data.config, &bundle, ledger_token).await {
        tracing::error!("tenant onboard: allocate on this participant failed: {e:#}");
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to allocate external party on this host: {e}"),
        });
    }

    // Report this host's view: `Completed` once its authorized mapping names it,
    // else `InProgress` (still a proposal here — other hosts must sign).
    let status = match host_onboarding_status(&data.config, &party_id).await {
        Ok(HostOnboardingStatus::Hosted) => WorkflowProgress::Completed,
        Ok(_) => WorkflowProgress::InProgress,
        Err(e) => {
            tracing::warn!("tenant onboard: post-allocate status read failed: {e:#}");
            WorkflowProgress::InProgress
        }
    };
    HttpResponse::Accepted().json(TenantOnboardResponse { status, party_id })
}

/// Onboarding status of a wallet-held party on THIS host, read from the
/// participant's topology. `party` must be the full party id (`{hint}::{fp}`).
/// `Completed` = this host's authorized `PartyToParticipant` names it;
/// `InProgress` = still a proposal here (this host has not finished signing);
/// 404 = no mapping at all. The wallet queries every host and aggregates.
#[utoipa::path(
    tag = "Tenant",
    params(("party" = String, Path, description = "Full party id")),
    responses(
        (status = 200, description = "Onboarding status on this host", body = WorkflowStatusResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "This host does not host this party", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/status")]
pub async fn tenant_status(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party = path.into_inner();

    match host_onboarding_status(&data.config, &party).await {
        Ok(HostOnboardingStatus::Hosted) => HttpResponse::Ok().json(WorkflowStatusResponse {
            status: WorkflowProgress::Completed,
            error: None,
        }),
        Ok(HostOnboardingStatus::Pending) => HttpResponse::Ok().json(WorkflowStatusResponse {
            status: WorkflowProgress::InProgress,
            error: None,
        }),
        Ok(HostOnboardingStatus::Absent) => HttpResponse::NotFound().json(ErrorResponse {
            error: format!("This host does not host external party {party}"),
        }),
        Err(e) => {
            tracing::error!("tenant status: topology read failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read onboarding status: {e}"),
            })
        }
    }
}

// ============================================================================
// Interactive submission + ACS (the party transacts)
// ============================================================================

/// Prepare an interactive submission for a wallet-held party — one create or
/// exercise command. Returns the prepared transaction + hash for the wallet to
/// sign.
///
/// For token-standard transfers use `prepare-transfer` / `prepare-accept`
/// instead: they resolve the registry choice context and disclosed contracts,
/// which a caller would otherwise have to assemble itself.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantPrepareSubmissionRequest,
    params(("party" = String, Path, description = "Party id")),
    responses(
        (status = 200, description = "Prepared transaction", body = TenantPrepareSubmissionResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/{party}/prepare-submission")]
pub async fn tenant_prepare_submission(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<TenantPrepareSubmissionRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party_id = match CantonId::parse(&path.into_inner()) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid party id: {e}"),
            });
        }
    };

    let command = match build_command(&body.command) {
        Ok(command) => command,
        Err(msg) => return HttpResponse::BadRequest().json(ErrorResponse { error: msg }),
    };
    let disclosed = match decode_disclosed_contracts(&body.disclosed_contracts) {
        Ok(contracts) => contracts,
        Err(msg) => return HttpResponse::BadRequest().json(ErrorResponse { error: msg }),
    };

    match prepare_for_party(&data, &party_id, command, disclosed).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(resp) => resp,
    }
}

/// Translate a tenant command into its Ledger API form.
fn build_command(command: &TenantCommand) -> std::result::Result<Command, String> {
    let inner = match command {
        TenantCommand::Create(create) => command::Command::Create(CreateCommand {
            template_id: Some(identifier(&create.template_id)),
            create_arguments: Some(json_to_record(&create.create_arguments)?),
        }),
        TenantCommand::Exercise(exercise) => command::Command::Exercise(ExerciseCommand {
            template_id: Some(identifier(&exercise.template_id)),
            contract_id: exercise.contract_id.clone(),
            choice: exercise.choice.clone(),
            choice_argument: Some(json_to_value(&exercise.choice_argument)?),
        }),
    };
    Ok(Command {
        command: Some(inner),
    })
}

fn identifier(id: &TenantTemplateId) -> Identifier {
    Identifier {
        package_id: id.package_id.clone(),
        module_name: id.module_name.clone(),
        entity_name: id.entity_name.clone(),
    }
}

fn decode_disclosed_contracts(
    contracts: &[TenantDisclosedContract],
) -> std::result::Result<Vec<DisclosedContract>, String> {
    contracts
        .iter()
        .map(|dc| {
            let created_event_blob = STANDARD.decode(&dc.created_event_blob).map_err(|e| {
                format!(
                    "disclosed contract {cid}: created_event_blob is not valid base64: {e}",
                    cid = dc.contract_id
                )
            })?;
            Ok(DisclosedContract {
                template_id: None,
                contract_id: dc.contract_id.clone(),
                created_event_blob,
                synchronizer_id: dc.synchronizer_id.clone(),
            })
        })
        .collect()
}

/// Prepare an interactive submission for `party_id` to sign.
///
/// Shared by `prepare-submission` and the transfer endpoints, which differ only in
/// how they arrive at the command: disclosed contracts attach here rather than at
/// execute time, because `ExecuteSubmissionRequest` has no field for them — the
/// prepared transaction Canton returns already embeds what it needs.
async fn prepare_for_party(
    data: &web::Data<AppState>,
    party_id: &CantonId,
    command: Command,
    disclosed_contracts: Vec<DisclosedContract>,
) -> std::result::Result<TenantPrepareSubmissionResponse, HttpResponse> {
    let token = node_ledger_token(data).await;
    let mut client = match utils::create_submission_client(&data.config, token).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("tenant prepare: client build failed: {e:#}");
            return Err(HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to connect to ledger: {e}"),
            }));
        }
    };

    let request = PrepareSubmissionRequest {
        user_id: String::new(),
        command_id: Uuid::new_v4().to_string(),
        commands: vec![command],
        min_ledger_time: None,
        max_record_time: None,
        act_as: vec![party_id.to_string()],
        read_as: vec![],
        disclosed_contracts,
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        verbose_hashing: false,
        prefetch_contract_keys: vec![],
        estimate_traffic_cost: None,
        hashing_scheme_version: None,
        taps_max_passes: None,
    };

    match client
        .prepare_submission(tonic::Request::new(request))
        .await
    {
        Ok(resp) => {
            let resp = resp.into_inner();
            let prepared_transaction = resp
                .prepared_transaction
                .map(|pt| STANDARD.encode(pt.encode_to_vec()))
                .unwrap_or_default();
            Ok(TenantPrepareSubmissionResponse {
                prepared_transaction,
                prepared_transaction_hash: STANDARD.encode(&resp.prepared_transaction_hash),
                hashing_scheme_version: resp.hashing_scheme_version,
            })
        }
        Err(status) => {
            tracing::error!("tenant prepare: RPC failed: {status}");
            Err(HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("PrepareSubmission failed: {status}"),
            }))
        }
    }
}

/// Execute a wallet-signed interactive submission for a party.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantExecuteSubmissionRequest,
    params(("party" = String, Path, description = "Party id")),
    responses(
        (status = 200, description = "Submission executed", body = ErrorResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/{party}/execute-submission")]
pub async fn tenant_execute_submission(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<TenantExecuteSubmissionRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party_id = match CantonId::parse(&path.into_inner()) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid party id: {e}"),
            });
        }
    };

    let prepared_transaction = match STANDARD.decode(&body.prepared_transaction) {
        Ok(bytes) => match PreparedTransaction::decode(bytes.as_slice()) {
            Ok(pt) => pt,
            Err(e) => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!("prepared_transaction is not a valid PreparedTransaction: {e}"),
                });
            }
        },
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("prepared_transaction is not valid base64: {e}"),
            });
        }
    };
    let signature = match STANDARD.decode(&body.signature) {
        Ok(bytes) => bytes,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("signature is not valid base64: {e}"),
            });
        }
    };

    let party_signatures = PartySignatures {
        signatures: vec![SinglePartySignatures {
            party: party_id.to_string(),
            signatures: vec![Signature {
                format: SignatureFormat::Concat as i32,
                signature,
                signed_by: body.signed_by.clone(),
                signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
            }],
        }],
    };

    let token = node_ledger_token(&data).await;
    let mut client = match utils::create_submission_client(&data.config, token).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("tenant execute-submission: client build failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to connect to ledger: {e}"),
            });
        }
    };

    let request = ExecuteSubmissionRequest {
        prepared_transaction: Some(prepared_transaction),
        party_signatures: Some(party_signatures),
        submission_id: Uuid::new_v4().to_string(),
        user_id: String::new(),
        hashing_scheme_version: body.hashing_scheme_version,
        min_ledger_time: None,
        deduplication_period: None,
    };

    match client
        .execute_submission(tonic::Request::new(request))
        .await
    {
        Ok(_) => HttpResponse::Ok().finish(),
        Err(status) => {
            tracing::error!("tenant execute-submission: RPC failed: {status}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("ExecuteSubmission failed: {status}"),
            })
        }
    }
}

/// List the active contracts owned by a wallet-held party.
#[utoipa::path(
    tag = "Tenant",
    params(("party" = String, Path, description = "Party id")),
    responses(
        (status = 200, description = "Active contracts", body = TenantAcsResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/acs")]
pub async fn tenant_acs(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party_id = match CantonId::parse(&path.into_inner()) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid party id: {e}"),
            });
        }
    };

    let token = node_ledger_token(&data).await;
    match fetch_party_acs(&data.config, token, &party_id).await {
        Ok(contracts) => HttpResponse::Ok().json(TenantAcsResponse { contracts }),
        Err(e) => {
            tracing::error!("tenant acs: query failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to query active contracts: {e}"),
            })
        }
    }
}

/// Stream the party's active contracts and project each into a [`TenantContract`].
async fn fetch_party_acs(
    config: &NodeConfig,
    token: Option<String>,
    party_id: &CantonId,
) -> Result<Vec<TenantContract>> {
    let mut client = utils::create_state_client(config, token).await?;
    let ledger_end = client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        // Verbose so created events carry record field labels, which makes
        // `create_arguments` render as a labelled JSON object.
        event_format: Some(party_event_format(
            party_id,
            vec![wildcard_filter(false)],
            true,
        )),
        stream_continuation_token: None,
    };

    let mut stream = client.get_active_contracts(request).await?.into_inner();
    let mut contracts = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            let template_id = created
                .template_id
                .map(|t| {
                    format!(
                        "{pkg}:{module}:{entity}",
                        pkg = t.package_id,
                        module = t.module_name,
                        entity = t.entity_name
                    )
                })
                .unwrap_or_default();
            let create_arguments = created
                .create_arguments
                .as_ref()
                .map(record_to_json)
                .unwrap_or(serde_json::Value::Null);
            contracts.push(TenantContract {
                contract_id: created.contract_id,
                template_id,
                create_arguments,
            });
        }
    }
    Ok(contracts)
}

// ============================================================================
// Token-standard transfers (the party holds and moves assets)
//
// Every asset move is an exercise whose interpretation reads a choice context
// only the instrument's registry can resolve, plus the contracts that context
// refers to. DPM resolves both and hands back a prepared transaction, so the
// wallet's job stays exactly what it is everywhere else: sign a hash. The same
// code path serves Canton Coin and utility instruments like CBTC — see
// [`token_standard::resolve`].
// ============================================================================

/// List a wallet-held party's balances, one row per instrument.
#[utoipa::path(
    tag = "Tenant",
    params(("party" = String, Path, description = "Party id")),
    responses(
        (status = 200, description = "Balances by instrument", body = TenantHoldingsResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/holdings")]
pub async fn tenant_holdings(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party_id = match parse_party(path.into_inner()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let token = node_ledger_token(&data).await;
    match queries::get_holdings(&data.config, &party_id, token, data.test_mode).await {
        Ok(holdings) => HttpResponse::Ok().json(TenantHoldingsResponse {
            holdings: holdings
                .into_iter()
                .map(|h| TenantHolding {
                    instrument_admin: h.instrument_admin.to_string(),
                    instrument_id: h.instrument_id,
                    total: h.amount.to_string(),
                    locked: h.locked_amount.to_string(),
                    available: (h.amount - h.locked_amount).to_string(),
                })
                .collect(),
        }),
        Err(e) => {
            tracing::error!("tenant holdings: query failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to query holdings: {e}"),
            })
        }
    }
}

/// List the inbound transfers awaiting this party's acceptance.
#[utoipa::path(
    tag = "Tenant",
    params(("party" = String, Path, description = "Party id")),
    responses(
        (status = 200, description = "Inbound transfer offers", body = TenantTransferOffersResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/transfer-offers")]
pub async fn tenant_transfer_offers(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party_id = match parse_party(path.into_inner()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let token = node_ledger_token(&data).await;
    match queries::get_open_transfer_instructions(&data.config, &party_id, token).await {
        Ok(instructions) => HttpResponse::Ok().json(TenantTransferOffersResponse {
            offers: instructions.iter().map(transfer_offer).collect(),
        }),
        Err(e) => {
            tracing::error!("tenant transfer-offers: query failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to query transfer offers: {e}"),
            })
        }
    }
}

fn transfer_offer(info: &TransferInstructionInfo) -> TenantTransferOffer {
    let expired = info.expires_at > 0 && info.expires_at <= chrono::Utc::now().timestamp();
    TenantTransferOffer {
        contract_id: info.contract_id.clone(),
        sender: info.sender.to_string(),
        instrument_admin: info.instrument_admin.to_string(),
        instrument_id: info.instrument_id.clone(),
        amount: info.amount.to_string(),
        acceptable: matches!(
            info.status,
            TransferInstructionStatus::PendingReceiverAcceptance
        ) && !expired,
        expires_at: info.expires_at,
    }
}

/// Prepare an outgoing token-standard transfer for the wallet to sign.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantTransferRequest,
    params(("party" = String, Path, description = "Sending party id")),
    responses(
        (status = 200, description = "Prepared transaction", body = TenantPrepareSubmissionResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 502, description = "The instrument's registry could not be reached", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/{party}/prepare-transfer")]
pub async fn tenant_prepare_transfer(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<TenantTransferRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party_id = match parse_party(path.into_inner()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let receiver = match parse_field_party(&body.receiver, "receiver") {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let instrument_admin = match parse_field_party(&body.instrument_admin, "instrument_admin") {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let amount = match DamlDecimal::parse(&body.amount) {
        Ok(amount) => amount,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("amount {:?} is not a valid decimal: {e}", body.amount),
            });
        }
    };
    if amount <= DamlDecimal::ZERO {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "amount must be greater than zero".to_string(),
        });
    }
    // A zero-hour window makes `execute_before` equal `requested_at`: the transfer
    // is expired the moment it exists, and for a two-step transfer that escrows the
    // sender's holdings until someone reclaims them. Refuse it rather than
    // construct it.
    if body.validity_window_hours == Some(0) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "validity_window_hours must be at least 1; a zero-hour window expires the \
                    transfer at the moment it is created, escrowing the funds until reclaimed"
                .to_string(),
        });
    }

    let token = node_ledger_token(&data).await;

    // Check the balance before Canton does. The factory's own refusal is a Daml
    // assertion — "collapseAction: amount must be positive and not exceed total
    // holding amount" — which tells the user nothing about what they hold or what
    // they asked for. Locked holdings are excluded, since they are escrowed against
    // a transfer nobody has accepted yet and cannot fund another.
    match queries::get_holdings(&data.config, &party_id, token.clone(), data.test_mode).await {
        Ok(holdings) => {
            let available = holdings
                .iter()
                .find(|h| {
                    h.instrument_admin == instrument_admin && h.instrument_id == body.instrument_id
                })
                .map(|h| h.amount - h.locked_amount)
                .unwrap_or(DamlDecimal::ZERO);
            if amount > available {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!(
                        "cannot send {amount} {instrument}: {party_id} has {available} available. \
                         Any remaining balance is locked against a transfer that has not been \
                         accepted yet.",
                        instrument = body.instrument_id
                    ),
                });
            }
        }
        // A failed balance read is not a reason to refuse the transfer; Canton
        // still enforces the real limit.
        Err(e) => tracing::warn!("tenant prepare-transfer: balance pre-check skipped: {e:#}"),
    }

    // The transfer factory rejects an empty `inputHoldingCids` outright, so when
    // the caller doesn't pin specific holdings, fund the transfer from every
    // unlocked holding for this instrument and let the choice return change.
    let input_holding_cids = if body.input_holding_cids.is_empty() {
        match queries::select_input_holdings(
            &data.config,
            &party_id,
            token.clone(),
            &instrument_admin,
            &body.instrument_id,
        )
        .await
        {
            Ok(cids) if cids.is_empty() => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!(
                        "{party_id} holds no unlocked {instrument} to fund this transfer",
                        instrument = body.instrument_id
                    ),
                });
            }
            Ok(cids) => cids,
            Err(e) => {
                tracing::error!("tenant prepare-transfer: holding selection failed: {e:#}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: format!("Failed to select input holdings: {e}"),
                });
            }
        }
    } else {
        body.input_holding_cids.clone()
    };

    // Captured once: the registry resolves the choice context for these exact
    // timestamps, so the context fetch and the submitted choice must agree.
    let now_micros = chrono::Utc::now().timestamp_micros();
    let validity = match body.validity_window_hours {
        Some(hours) => TransferValidity::from_now_with_window(
            now_micros,
            i64::from(hours).saturating_mul(60 * 60 * 1_000_000),
        ),
        None => TransferValidity::from_now(now_micros),
    };

    let args = token_standard::TransferArgs {
        sender: &party_id,
        receiver: &receiver,
        amount: &amount,
        instrument_admin: &instrument_admin,
        instrument_id: &body.instrument_id,
        input_holding_cids: &input_holding_cids,
        validity,
    };

    let endpoint =
        match token_standard::resolve(&data.config, &instrument_admin, &body.instrument_id).await {
            Ok(endpoint) => endpoint,
            Err(e) => {
                tracing::warn!("tenant prepare-transfer: registry resolution failed: {e:#}");
                return HttpResponse::BadGateway().json(ErrorResponse {
                    error: format!("Failed to locate the instrument's registry: {e}"),
                });
            }
        };
    let resolved = match token_standard::fetch_transfer_factory(&endpoint, &args).await {
        Ok(resolved) => resolved,
        Err(e) => {
            tracing::warn!("tenant prepare-transfer: factory lookup failed: {e:#}");
            return HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Failed to resolve the transfer factory: {e}"),
            });
        }
    };

    let choice_argument = match token_standard::transfer_choice_argument(&args, &resolved.context) {
        Ok(value) => value,
        Err(e) => {
            tracing::error!("tenant prepare-transfer: choice argument build failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to build the transfer choice argument: {e}"),
            });
        }
    };
    let disclosed = match to_proto_disclosed_contracts(&resolved.disclosed_contracts) {
        Ok(contracts) => contracts,
        Err(e) => {
            tracing::error!("tenant prepare-transfer: disclosed contracts invalid: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("The registry returned an unusable disclosed contract: {e}"),
            });
        }
    };

    let command = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(token_standard::transfer_factory_id()),
            contract_id: resolved.factory_cid,
            choice: token_standard::TRANSFER_CHOICE.to_string(),
            choice_argument: Some(choice_argument),
        })),
    };

    match prepare_for_party(&data, &party_id, command, disclosed).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(resp) => resp,
    }
}

/// Prepare acceptance of an inbound transfer for the wallet to sign.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAcceptTransferRequest,
    params(("party" = String, Path, description = "Receiving party id")),
    responses(
        (status = 200, description = "Prepared transaction", body = TenantPrepareSubmissionResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No such offer awaits this party", body = ErrorResponse),
        (status = 502, description = "The instrument's registry could not be reached", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/{party}/prepare-accept")]
pub async fn tenant_prepare_accept(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<TenantAcceptTransferRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
    let party_id = match parse_party(path.into_inner()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let instruction_cid = body.transfer_instruction_cid.trim();
    if instruction_cid.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "transfer_instruction_cid must not be empty".to_string(),
        });
    }

    let token = node_ledger_token(&data).await;

    // Look the offer up rather than trusting the request: it both confirms this
    // party is the receiver and yields the instrument, which is what decides
    // *which* registry serves the accept context.
    let offer = match queries::get_open_transfer_instructions(&data.config, &party_id, token).await
    {
        Ok(instructions) => instructions
            .into_iter()
            .find(|i| i.contract_id == instruction_cid),
        Err(e) => {
            tracing::error!("tenant prepare-accept: offer lookup failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to look up the transfer offer: {e}"),
            });
        }
    };
    let Some(offer) = offer else {
        return HttpResponse::NotFound().json(ErrorResponse {
            error: format!("no open transfer offer {instruction_cid} awaits {party_id}"),
        });
    };

    // Answer now rather than preparing a transaction that fails at interpretation.
    // The same `acceptable` the list endpoint reports decides it, so the two can
    // never disagree about whether an offer can be taken.
    let view = transfer_offer(&offer);
    if !view.acceptable {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "transfer offer {instruction_cid} cannot be accepted right now: it has either \
                 expired (deadline {expires_at}) or is still waiting on the registrar's own \
                 workflow",
                expires_at = view.expires_at
            ),
        });
    }

    let endpoint =
        match token_standard::resolve(&data.config, &offer.instrument_admin, &offer.instrument_id)
            .await
        {
            Ok(endpoint) => endpoint,
            Err(e) => {
                tracing::warn!("tenant prepare-accept: registry resolution failed: {e:#}");
                return HttpResponse::BadGateway().json(ErrorResponse {
                    error: format!("Failed to locate the instrument's registry: {e}"),
                });
            }
        };
    let resolved = match token_standard::fetch_accept_context(&endpoint, instruction_cid).await {
        Ok(resolved) => resolved,
        Err(e) => {
            tracing::warn!("tenant prepare-accept: context lookup failed: {e:#}");
            return HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Failed to resolve the accept context: {e}"),
            });
        }
    };

    let choice_argument = match token_standard::accept_choice_argument(&resolved.context) {
        Ok(value) => value,
        Err(e) => {
            tracing::error!("tenant prepare-accept: choice argument build failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to build the accept choice argument: {e}"),
            });
        }
    };
    let disclosed = match to_proto_disclosed_contracts(&resolved.disclosed_contracts) {
        Ok(contracts) => contracts,
        Err(e) => {
            tracing::error!("tenant prepare-accept: disclosed contracts invalid: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("The registry returned an unusable disclosed contract: {e}"),
            });
        }
    };

    let command = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(token_standard::transfer_instruction_id()),
            contract_id: instruction_cid.to_string(),
            choice: token_standard::ACCEPT_CHOICE.to_string(),
            choice_argument: Some(choice_argument),
        })),
    };

    match prepare_for_party(&data, &party_id, command, disclosed).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(resp) => resp,
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Parse a party id from the request path, rendering the failure as a 400.
fn parse_party(raw: String) -> std::result::Result<CantonId, HttpResponse> {
    CantonId::parse(&raw).map_err(|e| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("Invalid party id: {e}"),
        })
    })
}

/// Parse a party id from a named request-body field, naming the field in the error.
fn parse_field_party(raw: &str, field: &str) -> std::result::Result<CantonId, HttpResponse> {
    CantonId::parse(raw).map_err(|e| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("{field} {raw:?} is not a valid party id: {e}"),
        })
    })
}

/// The ledger token the tenant API's *transacting* half acts under.
///
/// Onboarding needs no ledger credential at all (it writes topology over the admin
/// API), but reading a party's contracts and relaying its signed submissions are
/// Ledger-API calls, and Canton scopes those to the caller's rights:
///
/// - `GetActiveContracts` and `PrepareSubmission` need `readAs` for the party
/// - `ExecuteSubmission` needs `executeAs` for the party
///
/// A freshly onboarded external party has no credential of its own, because it
/// exists only as topology. Some ledger user must therefore read and relay for it,
/// and that user needs `CanReadAsAnyParty` and `CanExecuteAsAnyParty`. Those rights
/// can read every party on the participant, so they get their own identity: the
/// tenant ledger user, configured by `DECPM_TENANT_LEDGER_*`.
///
/// The alternative was to add those rights to a dec party's user during `POST
/// /auth/grant-rights`. That coupled two unrelated things. An operator granting one
/// dec party its own rights would silently widen what that party's user can read,
/// and doing it per party widened every one of them. It also picked whichever party
/// sorted first, so granting on the wrong party left the tenant API without rights
/// and nothing said so.
///
/// Never `CanActAsAnyParty`: this node relays a submission the wallet signed and
/// must not be able to originate one.
///
/// `None` when the node configures no tenant ledger user. Canton then rejects the
/// call for want of authentication, rather than returning a silent empty result.
async fn node_ledger_token(data: &web::Data<AppState>) -> Option<String> {
    // Insecure mode mints one identity for everything, so no lookup is needed.
    {
        let auth = data.auth.read().await;
        if let Some(WorkflowAuth::Mock(registry)) = auth.as_ref() {
            return Some(registry.get_by_str("").await.get_token());
        }
    }

    match super::auth::tenant_ledger_token(&data.config, &data.http_client).await {
        Ok(token) => Some(token),
        Err(e) => {
            tracing::error!("tenant API has no usable ledger credential: {e:#}");
            None
        }
    }
}

/// Base64-decode a raw Ed25519 public key into its fixed 32-byte array, or the
/// 400 response to return.
fn decode_public_key(encoded: &str) -> std::result::Result<[u8; 32], HttpResponse> {
    let bytes = STANDARD.decode(encoded).map_err(|e| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("public_key is not valid base64: {e}"),
        })
    })?;
    bytes.try_into().map_err(|b: Vec<u8>| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("public_key must be 32 bytes, got {len}", len = b.len()),
        })
    })
}

/// Map a JSON object to a Daml record (labels = keys). Errors if not an object.
fn json_to_record(v: &serde_json::Value) -> std::result::Result<Record, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "create_arguments must be a JSON object".to_string())?;
    let fields = obj
        .iter()
        .map(|(label, value)| {
            Ok(RecordField {
                label: label.clone(),
                value: Some(json_to_value(value)?),
            })
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    Ok(Record {
        record_id: None,
        fields,
    })
}

/// Best-effort JSON → Daml `Value` mapping (see the module note on limits).
fn json_to_value(v: &serde_json::Value) -> std::result::Result<Value, String> {
    let sum = match v {
        serde_json::Value::Null => value::Sum::Optional(Box::new(Optional { value: None })),
        serde_json::Value::Bool(b) => value::Sum::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => value::Sum::Int64(i),
            None => value::Sum::Numeric(n.to_string()),
        },
        serde_json::Value::String(s) => {
            if CantonId::parse(s).is_ok() {
                value::Sum::Party(s.clone())
            } else {
                value::Sum::Text(s.clone())
            }
        }
        serde_json::Value::Array(items) => {
            let elements = items
                .iter()
                .map(json_to_value)
                .collect::<std::result::Result<Vec<_>, String>>()?;
            value::Sum::List(List { elements })
        }
        serde_json::Value::Object(_) => value::Sum::Record(json_to_record(v)?),
    };
    Ok(Value { sum: Some(sum) })
}

/// Render a Daml record as a JSON object. Fields with an empty label (positional,
/// non-verbose responses) fall back to `_{index}` keys.
fn record_to_json(record: &Record) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (idx, f) in record.fields.iter().enumerate() {
        let key = if f.label.is_empty() {
            format!("_{idx}")
        } else {
            f.label.clone()
        };
        let val = f
            .value
            .as_ref()
            .map(value_to_json)
            .unwrap_or(serde_json::Value::Null);
        obj.insert(key, val);
    }
    serde_json::Value::Object(obj)
}

/// Render a Daml `Value` as JSON for ACS display.
fn value_to_json(v: &Value) -> serde_json::Value {
    use serde_json::{Value as J, json};
    match &v.sum {
        Some(value::Sum::Unit(())) => J::Null,
        Some(value::Sum::Bool(b)) => J::Bool(*b),
        Some(value::Sum::Int64(i)) => json!(i),
        Some(value::Sum::Date(d)) => json!(d),
        Some(value::Sum::Timestamp(t)) => json!(t),
        Some(value::Sum::Numeric(n)) => J::String(n.clone()),
        Some(value::Sum::Party(p)) => J::String(p.clone()),
        Some(value::Sum::Text(t)) => J::String(t.clone()),
        Some(value::Sum::ContractId(c)) => J::String(c.clone()),
        Some(value::Sum::Optional(opt)) => match opt.value.as_deref() {
            Some(inner) => value_to_json(inner),
            None => J::Null,
        },
        Some(value::Sum::List(list)) => J::Array(list.elements.iter().map(value_to_json).collect()),
        Some(value::Sum::Record(r)) => record_to_json(r),
        Some(value::Sum::Variant(var)) => {
            let inner = var.value.as_deref().map(value_to_json).unwrap_or(J::Null);
            json!({ "_variant": var.constructor, "value": inner })
        }
        Some(value::Sum::Enum(e)) => J::String(e.constructor.clone()),
        Some(value::Sum::TextMap(_)) | Some(value::Sum::GenMap(_)) => {
            json!({ "_unsupported": "map" })
        }
        _ => J::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(hint: &str, tag: u8) -> CantonId {
        let namespace = format!("1220{}", format!("{tag:02x}").repeat(32));
        match CantonId::parse(&format!("{hint}::{namespace}")) {
            Ok(id) => id,
            Err(e) => panic!("test party id must parse: {e}"),
        }
    }

    fn offer(status: TransferInstructionStatus, expires_at: i64) -> TransferInstructionInfo {
        let Ok(amount) = DamlDecimal::parse("1.5") else {
            panic!("1.5 is a valid decimal");
        };
        TransferInstructionInfo {
            contract_id: "00instruction".to_string(),
            sender: party("bob", 0xbb),
            receiver: party("alice", 0xaa),
            amount,
            instrument_admin: party("DSO", 0xdd),
            instrument_id: "Amulet".to_string(),
            status,
            pending_actions: Vec::new(),
            expires_at,
        }
    }

    /// `acceptable` is what `prepare-accept` now refuses on, so it has to mean
    /// exactly "accepting this would succeed" — the list endpoint and the prepare
    /// endpoint read the same field for that reason.
    #[test]
    fn only_a_live_offer_awaiting_this_party_is_acceptable() {
        let future = chrono::Utc::now().timestamp() + 3600;
        let past = chrono::Utc::now().timestamp() - 1;

        assert!(
            transfer_offer(&offer(
                TransferInstructionStatus::PendingReceiverAcceptance,
                future
            ))
            .acceptable,
            "an unexpired offer waiting on this party can be accepted"
        );
        assert!(
            !transfer_offer(&offer(
                TransferInstructionStatus::PendingReceiverAcceptance,
                past
            ))
            .acceptable,
            "an expired offer cannot: Daml refuses the accept"
        );
        assert!(
            !transfer_offer(&offer(
                TransferInstructionStatus::PendingInternalWorkflow,
                future
            ))
            .acceptable,
            "an offer still waiting on the registrar cannot"
        );
        // No deadline at all is not treated as already expired.
        assert!(
            transfer_offer(&offer(
                TransferInstructionStatus::PendingReceiverAcceptance,
                0
            ))
            .acceptable,
            "a missing deadline must not read as expired"
        );
    }
}
