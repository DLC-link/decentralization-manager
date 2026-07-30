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
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, CreateCommand, CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest,
    GetLedgerEndRequest, Identifier, List, Optional, Record, RecordField, Signature,
    SignatureFormat, SigningAlgorithmSpec, Value, WildcardFilter, command, cumulative_filter,
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
        AppState,
        middleware::require_tenant_api_key,
        types::{
            ErrorResponse, TenantAcsResponse, TenantContract, TenantExecuteSubmissionRequest,
            TenantOnboardRequest, TenantOnboardResponse, TenantPrepareRequest,
            TenantPrepareResponse, TenantPrepareSubmissionRequest, TenantPrepareSubmissionResponse,
            WorkflowProgress, WorkflowStatusResponse,
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

use super::{governance::get_party_token, workflows::validate_confirmation_threshold};

// ============================================================================
// Onboarding (wallet-driven)
// ============================================================================

/// Prepare the onboarding topology for a wallet-held external party: DPM relays
/// the wallet's public key to Canton and returns the unsigned topology + the
/// multi-hash for the wallet to sign.
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

    // No run is registered here — the wallet signs the returned multi-hash and
    // calls `/v0/tenant/onboard` next.
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
            multi_hash: STANDARD.encode(&prep.multi_hash),
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

/// Onboard a wallet-held external party from its signed topology: DPM allocates
/// the party from the wallet-signed bundle on its own participant and fans the
/// bundle out to the hosting peers.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantOnboardRequest,
    responses(
        (status = 202, description = "Onboarding started", body = TenantOnboardResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 409, description = "Incompatible peer or duplicate run", body = ErrorResponse),
        (status = 422, description = "Selected peers are not mutually meshed", body = ErrorResponse)
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
    let signature = match STANDARD.decode(&body.multi_hash_signature) {
        Ok(bytes) => bytes,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("multi_hash_signature is not valid base64: {e}"),
            });
        }
    };
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

    let party_id = format!("{hint}::{fp}", hint = body.party_hint, fp = body.signed_by);
    let bundle = ExternalPartyAllocatePayload {
        party_id: party_id.clone(),
        topology_transactions,
        signature,
        signed_by: body.signed_by.clone(),
    };

    // Allocate on THIS participant only. The wallet calls `/onboard` on every
    // host itself; no host relays to another. Canton keeps the topology a
    // proposal until the last host signs, and `allocate_party` treats
    // `ALREADY_EXISTS` as success, so re-sends converge.
    if let Err(e) = allocate_party(&data.config, &bundle).await {
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

/// Prepare an interactive submission for a wallet-held party (single CREATE
/// command). Returns the prepared transaction + hash for the wallet to sign.
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

    let create_arguments = match json_to_record(&body.create_arguments) {
        Ok(record) => record,
        Err(msg) => return HttpResponse::BadRequest().json(ErrorResponse { error: msg }),
    };

    let command = Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some(Identifier {
                package_id: body.template_id.package_id.clone(),
                module_name: body.template_id.module_name.clone(),
                entity_name: body.template_id.entity_name.clone(),
            }),
            create_arguments: Some(create_arguments),
        })),
    };

    let token = get_party_token(&data, &party_id).await;
    let mut client = match utils::create_submission_client(&data.config, token).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("tenant prepare-submission: client build failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to connect to ledger: {e}"),
            });
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
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        verbose_hashing: false,
        prefetch_contract_keys: vec![],
        estimate_traffic_cost: None,
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
            HttpResponse::Ok().json(TenantPrepareSubmissionResponse {
                prepared_transaction,
                prepared_transaction_hash: STANDARD.encode(&resp.prepared_transaction_hash),
                hashing_scheme_version: resp.hashing_scheme_version,
            })
        }
        Err(status) => {
            tracing::error!("tenant prepare-submission: RPC failed: {status}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("PrepareSubmission failed: {status}"),
            })
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

    let token = get_party_token(&data, &party_id).await;
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

    let token = get_party_token(&data, &party_id).await;
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

    let mut filters_by_party = std::collections::HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
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

    let request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            // Verbose so created events carry record field labels, which makes
            // `create_arguments` render as a labelled JSON object.
            verbose: true,
        }),
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
// Helpers
// ============================================================================

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
