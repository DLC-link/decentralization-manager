use std::{
    collections::HashSet,
    sync::LazyLock,
    time::{Duration, Instant},
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, http::StatusCode, post, web};
use anyhow::Context;
use base64::Engine;
use prometheus::IntCounterVec;
use serde::Deserialize;
use sqlx::SqlitePool;
use tracing::Instrument;

use crate::server::ledger_paging::AcsCursor;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        Commands, CreatedEvent, DisclosedContract, SubmitAndWaitForTransactionRequest,
        SubmitAndWaitRequest, command_service_client::CommandServiceClient, value,
    },
    digitalasset::canton::topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, StoreId, Synchronizer, base_query,
        store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use decman_lib::catalog::commands::{
    build_cancel_domain_confirmation, build_cancel_proposal, build_cancel_self_confirmation,
    build_confirm_proposal, build_confirm_self_action, build_execute_proposal,
    build_execute_self_action, build_expire_domain_confirmation, build_expire_self_confirmation,
};
use decman_lib::catalog::proposals::credential::AcceptPaidCredential;
use decman_lib::catalog::proposals::custody::{
    AcceptTransfer, AcceptTransferWithContext, Transfer, TransferWithContext,
};
use decman_lib::catalog::proposals::rewards::SetupCouponReassignmentDelegation;
use decman_lib::catalog::proposals::utility::{
    RequestDevNetFeaturedAppRight, WithCredentialIdNonce,
};
use decman_lib::catalog::templates::{
    domain_confirmation_template, governable_action_interface, governance_rules_template,
    self_confirmation_template,
};
use decman_lib::framework::commands::{build_propose, first_created_contract_id};
use decman_lib::framework::encode::TransferValidity;
use decman_lib::framework::record::{
    extract_contract_id, field_contract_id, field_record, record_field,
};

use super::token_standard::{AmuletRulesContract, GovernanceQuery, fetch_amulet_rules};
use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, PackageConfig, default_package_config},
    db::{
        rows::{ChainAuditCacheRow, GovernanceMediatorTimeoutRow},
        schema::{SchemaRead, SchemaWrite},
    },
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    server::{
        AppState,
        audit::{AuditEvent, AuditParams, log_governance_audit, with_proposal_cid},
        chain_audit,
        ledger_rejection::{mediator_timeout, submission_failure},
        middleware::require_admin,
        queries::{
            ExpireTimeouts, GovernanceQueue, get_governance_confirmations,
            get_governance_state as query_governance_state, mark_stuck,
            resolve_contract_package_ref, select_input_holdings,
        },
        scan::{self, OpenMiningRound, ScanNotConfigured, ScanRefused},
        transfer_context::{
            AcceptTransferContext, ProposeTransferArgs, fetch as fetch_accept_transfer_context,
            fetch_factory_for_propose, fetch_proposal_created_event, has_template,
            maybe_fetch_for_proposal_event, needs_registry_context, to_proto_disclosed_contracts,
        },
        types::{
            ActionType, ActiveCouponReassignmentDelegation, AuditLogEntry, AuditLogQuery,
            AuditLogResponse, AuditScope, CancelConfirmationRequest, CancelProposalRequest,
            ChainAuditEntry, ChainAuditQuery, ChainAuditResponse, ConfirmActionRequest,
            DomainGovernanceAction, ErrorResponse, ExecuteActionRequest, ExpireConfirmationRequest,
            GovernanceResponse, GovernanceStateResponse, GovernanceType, KnownMember,
            KnownMembersResponse, MAX_RECENT_AUDIT_LIMIT, MessageResponse, ProposalType,
            ProposalsPageResponse, ProposeActionRequest, RewardStatusResponse,
            chain_audit_entry_from_row,
        },
    },
    utils,
};

// ============================================================================
// Read Endpoints
// ============================================================================

/// Drop each proposal's full payload from a confirmations response.
///
/// v1 lets every authenticated caller read any party on the node. Only an
/// admin may confirm or execute, so only an admin needs every field of what
/// they would sign. The summary fields stay for everyone.
fn withhold_payloads(domain_actions: &mut [DomainGovernanceAction]) {
    for action in domain_actions {
        action.proposal_payload = None;
        action.proposal_template_id = None;
    }
}

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
    http_req: HttpRequest,
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let may_act = require_admin(&http_req, data.admin_role.as_deref()).is_ok();

    let token = get_party_token(&data, party_id).await;

    let member_party_id = get_member_party_id(&data, party_id).await;
    let packages = packages();

    // Pull `(rules_contract_id, threshold)` off the active GovernanceRules /
    // rules contract. The Daml `ExecuteGovernanceAction`
    // choice gates on THIS threshold ("Enough member confirmations to
    // execute action") — not the decentralized-namespace topology
    // threshold, which is a separate value used for signing
    // PartyToParticipant updates. Falling back to the DNS threshold for
    // historical compatibility only when the gov state isn't reachable.
    let gov_state =
        match query_governance_state(&data.config, party_id, token.clone(), &packages).await {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!("Failed to fetch active rules contract: {e}");
                None
            }
        };
    let threshold = match &gov_state {
        Some(state) => state.threshold as usize,
        None => get_party_threshold(&data, party_id).await.unwrap_or(2),
    };

    let batch = query.limit.unwrap_or(25).clamp(1, 200);
    let from = match &query.cursor {
        Some(raw) if !raw.is_empty() => decode_cursor(raw),
        _ => None,
    };

    let timeouts = match data.db.get_mediator_timeouts(party_id).await {
        Ok(rows) => expire_timeouts(rows),
        Err(e) => {
            tracing::error!("Failed to read governance mediator timeouts: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read governance mediator timeouts: {e}"),
            });
        }
    };

    match get_governance_confirmations(
        &data.config,
        party_id,
        threshold,
        gov_state.as_ref(),
        token,
        &packages,
        batch,
        from,
        &timeouts,
    )
    .await
    {
        // A read that left cards out would show as a shorter list with no
        // warning. Answer it as the failed read it is.
        Ok(GovernanceQueue {
            complete: false, ..
        }) => HttpResponse::InternalServerError().json(ErrorResponse {
            error: FEED_READ_FAILED.to_string(),
        }),
        Ok(GovernanceQueue {
            mut actions,
            mut domain_actions,
            next_cursor,
            ..
        }) => {
            mark_stuck(
                &mut actions,
                &mut domain_actions,
                &timeouts,
                chrono::Utc::now().timestamp(),
            );
            if !may_act {
                withhold_payloads(&mut domain_actions);
            }
            HttpResponse::Ok().json(GovernanceResponse {
                actions,
                next_cursor: next_cursor.as_ref().and_then(encode_cursor),
                domain_actions,
                threshold,
                member_party_id,
                rules_contract_id: gov_state.as_ref().map(|state| state.contract_id.clone()),
                gov_core_out_of_date: gov_state.as_ref().is_some_and(|state| state.out_of_date),
                gov_core_package_ref: gov_state.and_then(|state| state.package_ref),
            })
        }
        Err(e) => {
            tracing::error!("Failed to fetch governance confirmations: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: FEED_READ_FAILED.to_string(),
            })
        }
    }
}

/// The answer to a confirmations read that failed. The cause, which can
/// name an address, goes to the server log only.
const FEED_READ_FAILED: &str =
    "Failed to fetch governance confirmations: a ledger read failed. See the DecMan server log.";

/// Get governance state (GovernanceRules contract state)
/// Query parameters for the paged proposals feed.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ProposalsQuery {
    pub party_id: CantonId,
    /// Proposals per batch. Defaults to 50, capped at 200.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Opaque `next_cursor` from the previous batch. Absent starts at the top.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Default and ceiling for one batch. The ceiling is what keeps a client from
/// asking for a party's whole proposal history in one request (#424).
const PROPOSALS_BATCH_DEFAULT: usize = 50;
const PROPOSALS_BATCH_MAX: usize = 200;

/// The stored expire timeouts as the parties each one named. A row whose
/// list does not decode names nobody, so it marks nothing.
fn expire_timeouts(rows: Vec<GovernanceMediatorTimeoutRow>) -> ExpireTimeouts {
    rows.into_iter()
        .map(|row| {
            let unresponsive = serde_json::from_str(&row.unresponsive).unwrap_or_else(|e| {
                tracing::warn!(
                    contract_id = row.contract_id,
                    "Unreadable expire timeout: {e}"
                );
                HashSet::new()
            });
            (row.contract_id, unresponsive)
        })
        .collect()
}

fn encode_cursor(cursor: &AcsCursor) -> Option<String> {
    serde_json::to_vec(cursor)
        .ok()
        .map(|bytes| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(raw: &str) -> Option<AcsCursor> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// One batch of a party's open proposals.
///
/// Paged because a party can hold tens of thousands: reading them all in one
/// request either times out or exhausts the node (#424). Carries no
/// confirmation counts — resolving those means reading every
/// `GovernanceConfirmation`, which is the same unbounded scan.
#[utoipa::path(
    tag = "Governance",
    params(ProposalsQuery),
    responses(
        (status = 200, description = "A batch of proposals", body = ProposalsPageResponse),
        (status = 400, description = "Malformed cursor", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/proposals")]
pub async fn get_proposals_page(
    data: web::Data<AppState>,
    query: web::Query<ProposalsQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let limit = query
        .limit
        .unwrap_or(PROPOSALS_BATCH_DEFAULT)
        .clamp(1, PROPOSALS_BATCH_MAX);

    let cursor = match &query.cursor {
        Some(raw) if !raw.is_empty() => match decode_cursor(raw) {
            Some(c) => Some(c),
            None => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: "cursor is not one this endpoint issued".to_string(),
                });
            }
        },
        _ => None,
    };

    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    // One contract, so cheap next to the batch. Lets a card render
    // "n of threshold" without the client fetching the rules separately.
    let threshold = query_governance_state(&data.config, party_id, token.clone(), &packages)
        .await
        .ok()
        .flatten()
        .map(|state| state.threshold.max(0) as usize)
        .unwrap_or(0);

    match crate::server::queries::page_proposal_summaries(
        &data.config,
        party_id,
        token,
        &packages,
        limit,
        cursor,
    )
    .await
    {
        Ok((proposals, next)) => HttpResponse::Ok().json(ProposalsPageResponse {
            proposals,
            threshold,
            next_cursor: next.as_ref().and_then(encode_cursor),
        }),
        Err(e) => {
            tracing::error!(%party_id, "Failed to read a proposals batch: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read proposals: {e}"),
            })
        }
    }
}

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
    let packages = packages();

    match query_governance_state(&data.config, party_id, token, &packages).await {
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

/// Get paginated governance audit trail
///
/// Scoped to `party_id` when given. Without it, returns this node's newest
/// entries across every party, which is how the notification feed fills its
/// Completed section without one ledger read per party.
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
    let rows = match &query.party_id {
        Some(party_id) => {
            data.db
                .get_governance_audit(party_id, query.limit, query.offset)
                .await
        }
        None => {
            data.db
                .get_recent_governance_audit(
                    query.limit.clamp(1, MAX_RECENT_AUDIT_LIMIT),
                    query.offset.max(0),
                )
                .await
        }
    };
    match rows {
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

/// Wrap a page of audit entries, deriving the cursor for the next (older) page.
///
/// `has_more` is passed in rather than inferred from the row count: a page can
/// hold more than `limit` rows (it is extended to the end of an offset group),
/// so counting rows would hand out a cursor to a page that turns out empty.
fn chain_audit_response(entries: Vec<ChainAuditEntry>, has_more: bool) -> ChainAuditResponse {
    let total_returned = entries.len();
    let next_before_offset = has_more.then(|| entries.last().map(|e| e.offset)).flatten();

    ChainAuditResponse {
        entries,
        total_returned,
        next_before_offset,
    }
}

/// The cached answer for a page request, or `None` when the cache cannot answer
/// it and the read has to reach Canton.
///
/// An empty result is a miss rather than an answer: the cache only holds the
/// pages fetched so far, so paging past its tail (or a cold start) must not be
/// reported as an exhausted trail.
fn cached_chain_audit_page(
    rows: Vec<ChainAuditCacheRow>,
    limit: usize,
) -> Option<ChainAuditResponse> {
    if rows.is_empty() {
        return None;
    }

    // The cache cannot prove the trail is exhausted either, so a full page is
    // reported as "more" and the next request falls through to Canton — better
    // a spare empty page than hiding entries that do exist.
    let has_more = rows.len() >= limit;
    let entries: Vec<ChainAuditEntry> = rows.into_iter().map(chain_audit_entry_from_row).collect();

    Some(chain_audit_response(entries, has_more))
}

/// Get on-chain audit entries for a party.
///
/// Defaults to the governance scope, served from cache; pass `refresh=true` to
/// fetch from Canton and update the cache. `scope=all` returns every ledger
/// event the party witnesses and is always read live — the cache holds
/// governance-scoped pages, and mixing the two under one party key would let
/// an `all` read answer a governance request.
#[utoipa::path(
    tag = "Governance",
    params(ChainAuditQuery),
    responses(
        (status = 200, description = "On-chain audit entries for the requested scope", body = ChainAuditResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/chain-audit")]
pub async fn get_governance_chain_audit(
    data: web::Data<AppState>,
    query: web::Query<ChainAuditQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    // `limit` is caller-supplied, so it is bounded here rather than trusted:
    // one page must not be able to ask for the whole retained ledger.
    let limit = query.clamped_limit();

    // A zero-row page needs no ledger reads.
    if limit == 0 {
        return HttpResponse::Ok().json(chain_audit_response(Vec::new(), false));
    }

    let cacheable = query.scope == AuditScope::Governance;

    if !query.refresh && cacheable {
        match data
            .db
            .get_chain_audit_cache(party_id, limit as i64, query.before_offset)
            .await
        {
            Ok(rows) => {
                if let Some(response) = cached_chain_audit_page(rows, limit) {
                    return HttpResponse::Ok().json(response);
                }
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

    match chain_audit::get_chain_audit(
        &data.config,
        party_id,
        token,
        &pkgs,
        query.scope,
        limit,
        query.before_offset,
    )
    .await
    {
        Ok(page) => {
            if cacheable {
                // Save to cache in background
                let pool = data.db.clone();
                let pid = party_id.clone();
                let cached = page.entries.clone();
                tokio::spawn(async move {
                    chain_audit::save_chain_audit_cache(&pool, &pid, &cached).await;
                });
            }

            HttpResponse::Ok().json(chain_audit_response(page.entries, page.has_more))
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

static ACTIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_governance_actions_total",
        "Governance action attempts by step and outcome. error covers failures before and during ledger submission, not only ledger rejections. A request refused up front, such as by validation, does not count.",
        &["step", "outcome"]
    )
    .expect("metric name is a unique literal")
});

/// A governance write this node attempts for its member party.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionStep {
    Propose,
    Confirm,
    Execute,
    Expire,
    CancelConfirmation,
    CancelProposal,
}

impl ActionStep {
    const ALL: [Self; 6] = [
        Self::Propose,
        Self::Confirm,
        Self::Execute,
        Self::Expire,
        Self::CancelConfirmation,
        Self::CancelProposal,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Propose => "propose",
            Self::Confirm => "confirm",
            Self::Execute => "execute",
            Self::Expire => "expire",
            Self::CancelConfirmation => "cancel_confirmation",
            Self::CancelProposal => "cancel_proposal",
        }
    }

    fn audit_event(self) -> AuditEvent {
        match self {
            Self::Propose => AuditEvent::Propose,
            Self::Confirm => AuditEvent::Confirm,
            Self::Execute => AuditEvent::Execute,
            Self::Expire => AuditEvent::Expire,
            Self::CancelConfirmation => AuditEvent::Cancel,
            Self::CancelProposal => AuditEvent::CancelProposal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionOutcome {
    Ok,
    Error,
}

impl ActionOutcome {
    const ALL: [Self; 2] = [Self::Ok, Self::Error];

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// Exports every step and outcome at zero, so a rate query reads 0 rather than
/// nothing before the first attempt.
pub(crate) fn register_metrics() {
    for step in ActionStep::ALL {
        for outcome in ActionOutcome::ALL {
            ACTIONS
                .with_label_values(&[step.label(), outcome.label()])
                .inc_by(0);
        }
    }
}

/// Counts one attempt. An attempt starts at the `execute_*` call for a choice
/// step, and at the Ledger API connect for propose. Its `error` can come before
/// the submission itself: a failed connect, an unconfigured package, or a blob
/// that does not decode. A request the handler refuses before that point is a
/// caller error and does not count.
fn count_action(step: ActionStep, outcome: ActionOutcome) {
    ACTIONS
        .with_label_values(&[step.label(), outcome.label()])
        .inc();
}

// ============================================================================
// Spawned Writes
// ============================================================================

/// How long a governance write waits for the ledger, across all its
/// submissions. Without a deadline, a write whose caller hung up waits until
/// Canton stops tracking the command. The deployment guide asks a proxy to
/// wait longer than this, so DecMan's own answer reaches the caller.
const SUBMISSION_DEADLINE: Duration = Duration::from_secs(130);

/// When a write whose first submission starts now stops waiting.
fn write_deadline() -> Instant {
    Instant::now() + SUBMISSION_DEADLINE
}

/// A Ledger API request as `token` that waits for the ledger until
/// `deadline`. A write's later submissions get only what is left of it.
fn ledger_request<T>(message: T, token: &str, deadline: Instant) -> Result<tonic::Request<T>> {
    let mut request = tonic::Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    request.set_timeout(deadline.saturating_duration_since(Instant::now()));
    Ok(request)
}

/// The audit row of a governance write, before its outcome is known.
struct WriteRecord {
    step: ActionStep,
    party_id: CantonId,
    member_party_id: CantonId,
    governance_type: GovernanceType,
    action_summary: String,
    details: String,
}

impl WriteRecord {
    /// Counts the write, logs its failure, and writes its audit row. Returns
    /// the failure for the caller once the row is written, so a caller that
    /// reads the audit trail right after the answer finds it.
    async fn finish(self, db: SqlitePool, submitted: Submitted) -> Option<(StatusCode, String)> {
        let (outcome, status) = match submitted.failure {
            Some(_) => (ActionOutcome::Error, "failed"),
            None => (ActionOutcome::Ok, "success"),
        };
        count_action(self.step, outcome);
        if let Some((_, error)) = &submitted.failure {
            tracing::error!("{error}");
        }
        let details = match &submitted.proposal_cid {
            Some(cid) => with_proposal_cid(&self.details, cid),
            None => self.details,
        };
        log_governance_audit(
            &db,
            AuditParams {
                event_type: self.step.audit_event(),
                party_id: self.party_id,
                member_party_id: self.member_party_id,
                governance_type: self.governance_type,
                action_summary: self.action_summary,
                details,
                status,
                error_message: submitted.failure.as_ref().map(|(_, error)| error.clone()),
            },
        )
        .await;
        submitted.failure
    }
}

/// How a governance write's submission ended.
struct Submitted {
    /// The status and message for the caller when the write failed.
    failure: Option<(StatusCode, String)>,
    /// The proposal a propose created. Its audit row names it even when the
    /// proposer's own confirmation failed afterwards.
    proposal_cid: Option<String>,
}

impl Submitted {
    fn failed(status: StatusCode, error: String) -> Self {
        Self {
            failure: Some((status, error)),
            proposal_cid: None,
        }
    }

    /// The end of a submission whose error `failure` turns into the caller's
    /// status and message.
    fn of(result: Result, failure: impl FnOnce(&anyhow::Error) -> (StatusCode, String)) -> Self {
        Self {
            failure: result.err().map(|e| failure(&e)),
            proposal_cid: None,
        }
    }

    fn of_proposal(self, proposal_cid: String) -> Self {
        Self {
            proposal_cid: Some(proposal_cid),
            ..self
        }
    }
}

/// Runs a governance write to its end on its own task, then counts it and
/// writes its audit row. Answers with `success`, or with the failure.
///
/// A proxy can hang up before the mediator decides, and the server then drops
/// the handler. The write still ends, so its audit row records the cause. The
/// submission runs on a further task, so a panic in it still leaves a failed
/// row.
async fn run_write<F>(
    db: SqlitePool,
    record: WriteRecord,
    success: &'static str,
    submission: F,
) -> HttpResponse
where
    F: Future<Output = Submitted> + Send + 'static,
{
    let step = record.step.label();
    // Every line the submission logs, the full error chain among them, then
    // names the party and the write, so an operator can find it.
    let span = tracing::warn_span!(
        "governance_write",
        party_id = %record.party_id,
        write = %step
    );
    let submission = submission.instrument(span);
    let write = tokio::spawn(async move {
        let submitted = tokio::spawn(submission).await.unwrap_or_else(|e| {
            tracing::error!("The {step} submission ended without an outcome: {e}");
            Submitted::failed(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "The {step} submission ended without an outcome. See the DecMan server log."
                ),
            )
        });
        record.finish(db, submitted).await
    });
    let failure = write.await.unwrap_or_else(|e| {
        tracing::error!("The {step} write ended without an outcome: {e}");
        Some((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("The {step} write ended without an outcome. See the DecMan server log."),
        ))
    });
    match failure {
        None => HttpResponse::Ok().json(MessageResponse {
            message: success.to_string(),
        }),
        Some((status, error)) => HttpResponse::build(status).json(ErrorResponse { error }),
    }
}

/// True when this proposal, on a decparty that already has a live
/// `CouponReassignmentDelegation`, would create a SECOND one.
///
/// Canton cannot enforce the per-decparty singleton — contract keys do not give
/// cross-participant uniqueness — and `executeImpl` cannot query the ACS, so the
/// propose boundary is the only place the accident is catchable. Replacing an
/// existing delegation is fine; leaving `prior_delegation` empty while one is
/// live is not, because an assigner can still pay the superseded split.
///
/// This is the proposal shape alone — the handler pairs it with the ACS read.
/// Keeping the shape test separate is what lets `propose_action` skip that read
/// entirely for every other proposal type, which would otherwise pay a
/// `GetLedgerEnd` plus a `GetActiveContracts` for an answer that cannot change.
fn may_create_second_delegation(proposal: &ProposalType) -> bool {
    matches!(
        proposal,
        ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
            prior_delegation: None,
            ..
        })
    )
}

/// A fresh nonce for a proposal whose template mints governance
/// credentials: the first 12 hex digits of a v4 UUID, which carry 48 random
/// bits. The template folds it into each credential id, so two such
/// proposals that mint for one subject at one ledger time still mint
/// distinct ids.
fn credential_id_nonce() -> String {
    let mut nonce = uuid::Uuid::new_v4().simple().to_string();
    nonce.truncate(12);
    nonce
}

/// Propose a domain governance action (creates a GovernableAction proposal contract)
#[utoipa::path(
    tag = "Governance",
    request_body = ProposeActionRequest,
    responses(
        (status = 200, description = "Proposal created", body = MessageResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 409, description = "A CouponReassignmentDelegation is already active; set prior_delegation to replace it", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 503, description = "Node not provisioned for this proposal: the target package is not configured (the response says so, and whether the proposal was already created), or it cannot be confirmed whether a CouponReassignmentDelegation is already active", body = ErrorResponse),
        (status = 504, description = "The mediator timed out waiting for confirmations; the error names the parties that did not answer", body = ErrorResponse)
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
    if let Err(msg) = body.proposal.validate(&body.party_id) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: msg.to_string(),
        });
    }
    let party_id = &body.party_id;
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    // Canton cannot enforce the per-decparty singleton, so the propose boundary
    // is the only place a second CouponReassignmentDelegation is catchable.
    //
    // Match on the proposal type BEFORE reading the ledger: only one variant can
    // trip this guard, and the read is a GetLedgerEnd plus a GetActiveContracts
    // that every other propose would otherwise pay for nothing.
    if may_create_second_delegation(&body.proposal) {
        match crate::server::reward_automation::active_delegation(
            &data.config,
            &packages(),
            data.test_mode,
            party_id,
            &token,
        )
        .await
        {
            Ok(Some(active)) => {
                return HttpResponse::Conflict().json(ErrorResponse {
                    error: format!(
                        "a CouponReassignmentDelegation is already active ({}); set \
                         prior_delegation to replace it — creating a second one stops \
                         assignment entirely",
                        active.cid
                    ),
                });
            }
            Ok(None) => {}
            // Do not swallow this. The guard exists for exactly the state a
            // failed read leaves us blind to, so let the proposer see it rather
            // than admitting a second delegation on a transport blip.
            Err(e) => {
                tracing::warn!(%party_id, error = %e, "delegation singleton check failed");
                return HttpResponse::ServiceUnavailable().json(ErrorResponse {
                    error: format!(
                        "cannot confirm whether a CouponReassignmentDelegation is already \
                         active, so this proposal is refused rather than risk a second \
                         one: {e}"
                    ),
                });
            }
        }
    }

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
        ProposalType::Transfer(Transfer {
            validity_window_hours: Some(hours),
            ..
        }) => TransferValidity::from_now_with_window(
            now_micros,
            i64::from(*hours).saturating_mul(60 * 60 * 1_000_000),
        ),
        _ => TransferValidity::from_now(now_micros),
    };

    let mut resolved_proposal = body.proposal.clone();
    let transfer_choice_context = match &mut resolved_proposal {
        ProposalType::AcceptTransfer(AcceptTransfer {
            transfer_instruction_cid,
        }) => match fetch_accept_transfer_context(
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
        ProposalType::Transfer(Transfer {
            transfer_factory_cid,
            receiver,
            amount,
            instrument_id,
            input_holding_cids,
            ..
        }) if needs_registry_context(
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

    // The two transfer variants carry runtime data that is not a payload
    // field — the registry choice context, the validity window, and the
    // on-chain `sender` (the governance party) — so they submit through
    // their wrapper structs. The credential-minting variants carry a fresh
    // credential id nonce through theirs. Every other variant is its own
    // complete payload, reached through `grpc_payload()` without
    // re-listing every arm.
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match &resolved_proposal {
        ProposalType::Transfer(t) => build_propose(
            &TransferWithContext {
                transfer: t,
                sender: party_id,
                context: transfer_choice_context.as_ref().map(|r| &r.context),
                validity: transfer_validity,
            },
            party_id,
            &member_party_id,
            &packages,
            command_id,
        ),
        ProposalType::AcceptTransfer(a) => build_propose(
            &AcceptTransferWithContext {
                accept: a,
                context: transfer_choice_context.as_ref().map(|r| &r.context),
            },
            party_id,
            &member_party_id,
            &packages,
            command_id,
        ),
        other => match (other.credential_minting_payload(), other.grpc_payload()) {
            (Some(p), _) => build_propose(
                &WithCredentialIdNonce {
                    proposal: p,
                    nonce: &credential_id_nonce(),
                },
                party_id,
                &member_party_id,
                &packages,
                command_id,
            ),
            (None, Some(p)) => build_propose(p, party_id, &member_party_id, &packages, command_id),
            // No variant reaches this today: `grpc_payload` returns `None`
            // only for the two transfer variants, and both match above. A
            // new variant that returns `None` gets a 500, not a panic.
            (None, None) => {
                tracing::error!("Proposal variant has no gRPC payload: {other:?}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "Unsupported proposal type".to_string(),
                });
            }
        },
    };
    // An unconfigured package is a provisioning gap, not bad input: 503 with
    // the lib's own `"<pkg> package not configured"` text, byte-identical to
    // what the hand-written package match returned.
    let commands = match commands {
        Ok(c) => c,
        Err(e @ decman_lib::Error::PackageNotConfigured(_)) => {
            return HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: e.to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Failed to build proposal create arguments: {e}"),
            });
        }
    };

    let record = WriteRecord {
        step: ActionStep::Propose,
        party_id: party_id.clone(),
        member_party_id: member_party_id.clone(),
        governance_type: GovernanceType::CoreDomain,
        action_summary: crate::server::audit::proposal_summary(&body.proposal),
        details: serde_json::to_string(&*body).unwrap_or_default(),
    };
    let db = data.db.clone();
    let submission = submit_proposal(
        data,
        commands,
        token,
        record.member_party_id.clone(),
        body.into_inner(),
        write_deadline(),
    );
    run_write(db, record, "Proposal created and confirmed", submission).await
}

/// Creates a proposal from `commands`, then confirms it as the proposer.
///
/// Both submissions, and the ledger read between them, share one budget that
/// ends at `deadline`, so a propose waits no longer than any other write.
async fn submit_proposal(
    data: web::Data<AppState>,
    commands: Commands,
    token: String,
    member_party_id: CantonId,
    body: ProposeActionRequest,
    deadline: Instant,
) -> Submitted {
    let channel = match data.config.ledger_channel().await {
        Ok(channel) => channel,
        Err(e) => {
            let (status, error) = submission_failure("Failed to create proposal", &e);
            return Submitted::failed(status, error);
        }
    };

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    // Step 1: Create the proposal and get the contract ID back
    let create = SubmitAndWaitForTransactionRequest {
        commands: Some(commands),
        transaction_format: None,
    };
    let created: Result<_> = async {
        let request = ledger_request(create, &token, deadline)?;
        Ok(client.submit_and_wait_for_transaction(request).await?)
    }
    .await;
    let proposal_cid = match created {
        Ok(response) => {
            let transaction = response.into_inner().transaction;
            match transaction.as_ref().and_then(first_created_contract_id) {
                Some(cid) => cid,
                None => {
                    let update_id = transaction.map(|t| t.update_id).unwrap_or_default();
                    return Submitted::failed(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "Proposal created in update {update_id} but could not extract \
                             its contract ID. Find it in `governance pending` and confirm it \
                             there; do not re-propose."
                        ),
                    );
                }
            }
        }
        Err(e) => {
            let (status, error) = submission_failure("Failed to create proposal", &e);
            return Submitted::failed(status, error);
        }
    };

    tracing::info!("Proposal created with CID: {proposal_cid}");

    // Step 2: Immediately confirm the proposal as the proposer
    //
    // Reachable independently of the Step 1 checks: those only verify the
    // package the proposal type itself needs, so a rewards / token-custody /
    // utility proposal reaches here on a node with no governance_core. The
    // proposal is already on the ledger by this point, so the body has to say
    // so — the caller must not retry into a second proposal.
    let mut rules = match governance_rules_template(&packages()) {
        Ok(rules) => rules,
        Err(_) => {
            return Submitted::failed(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "Proposal {proposal_cid} was created but could not be confirmed: \
                     governance_core package not configured. Configure it and confirm \
                     the existing proposal; do not re-propose."
                ),
            )
            .of_proposal(proposal_cid);
        }
    };

    // The rules contract may be an out-of-date fallback living under an older
    // governance-core package — exercise it under its actual package ref.
    let package_ref = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        resolve_contract_package_ref(
            &data.config,
            &body.party_id,
            Some(token.clone()),
            &body.rules_contract_id,
            &rules.package_ref,
        ),
    )
    .await;
    rules.package_ref = match package_ref {
        Ok(package_ref) => package_ref,
        Err(_) => {
            return Submitted::failed(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Proposal created (CID: {proposal_cid}) but not confirmed: DecMan ran out \
                     of time while it read the governance rules, before it sent the \
                     confirmation. Confirm proposal {proposal_cid}; do not re-propose."
                ),
            )
            .of_proposal(proposal_cid);
        }
    };

    let confirm = SubmitAndWaitRequest {
        commands: Some(build_confirm_proposal(
            &rules,
            &body.rules_contract_id,
            &member_party_id,
            &body.party_id,
            &proposal_cid,
            uuid::Uuid::new_v4().to_string(),
        )),
    };
    let confirmed: Result = async {
        client
            .submit_and_wait(ledger_request(confirm, &token, deadline)?)
            .await?;
        Ok(())
    }
    .await;
    if confirmed.is_ok() {
        tracing::info!("Proposal {proposal_cid} confirmed by proposer");
    }
    Submitted::of(confirmed, |e| {
        let (status, error) = submission_failure(
            &format!("Proposal created (CID: {proposal_cid}) but confirmation failed"),
            e,
        );
        let advice = format!("Confirm proposal {proposal_cid}; do not re-propose.");
        (status, format!("{error} {advice}"))
    })
    .of_proposal(proposal_cid)
}

/// Reject an inline action the `core_self` path cannot carry, before any
/// ledger work happens.
///
/// The inline confirm/execute choices serialize a `GovernanceSelfAction`, so
/// only the self-management variants fit. Everything else in `ActionType`
/// exists for the read path and belongs on `POST /governance/propose`. Caught
/// here it is a clear 400; left to the serializer it would be a 500.
///
/// Also runs the action's own field validation (thresholds, timeouts) so a
/// malformed value surfaces as a 400 rather than a generic ledger submission
/// error.
///
/// Scoped to `core_self` deliberately. `core_domain` builds its choice from
/// `proposal_cid` and ignores `action` entirely, so both clients send a
/// deliberate `governance_set_threshold: 0` placeholder there for payload
/// symmetry — validating it would 400 every domain confirm and execute.
fn validate_inline_action(
    action: &ActionType,
    governance_type: GovernanceType,
) -> Result<(), String> {
    if !matches!(governance_type, GovernanceType::CoreSelf) {
        return Ok(());
    }
    if !action.is_governance_self_action() {
        return Err(format!(
            "action '{}' is not a governance self-management action; submit it as a domain \
             proposal via POST /governance/propose",
            crate::server::audit::action_summary(action)
        ));
    }
    action.validate().map_err(|e| e.to_string())
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
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 504, description = "The mediator timed out waiting for confirmations; the error names the parties that did not answer", body = ErrorResponse)
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
    if let Err(error) = validate_inline_action(&body.action, body.governance_type) {
        return HttpResponse::BadRequest().json(ErrorResponse { error });
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let record = WriteRecord {
        step: ActionStep::Confirm,
        party_id: party_id.clone(),
        member_party_id: member_party_id.clone(),
        governance_type: body.governance_type,
        action_summary: crate::server::audit::action_summary(&body.action),
        details: serde_json::to_string(&*body).unwrap_or_default(),
    };
    let db = data.db.clone();
    let body = body.into_inner();
    let submission = async move {
        let result =
            execute_confirm_action(&data.config, &body, &token, &member_party_id, &packages())
                .await;
        Submitted::of(result, |e| {
            submission_failure("Failed to submit confirmation", e)
        })
    };
    run_write(
        db,
        record,
        "Confirmation submitted successfully",
        submission,
    )
    .await
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
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 422, description = "A paid credential accept needs DECPM_SCAN_URL, which is not set", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 504, description = "The mediator timed out waiting for confirmations; the error names the parties that did not answer", body = ErrorResponse)
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
    if let Err(error) = validate_inline_action(&body.action, body.governance_type) {
        return HttpResponse::BadRequest().json(ErrorResponse { error });
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    // Redact disclosed contract blobs (can be very large) before storing in audit
    let mut redacted = body.clone();
    for dc in &mut redacted.disclosed_contracts {
        dc.blob = format!("<{} bytes>", dc.blob.len());
    }
    let record = WriteRecord {
        step: ActionStep::Execute,
        party_id: party_id.clone(),
        member_party_id: member_party_id.clone(),
        governance_type: body.governance_type,
        action_summary: crate::server::audit::action_summary(&body.action),
        details: serde_json::to_string(&redacted).unwrap_or_default(),
    };
    let db = data.db.clone();
    let body = body.into_inner();
    let submission = async move {
        let result = execute_confirmed_action(
            &data.config,
            &data.http_client,
            &body,
            &token,
            &member_party_id,
            &packages(),
        )
        .await;
        Submitted::of(result, execute_failure)
    };
    run_write(db, record, "Action executed successfully", submission).await
}

/// The execute handler's status and message for a failed execute. A node
/// without `DECPM_SCAN_URL` cannot disclose what a paid credential accept
/// reads. That is a setting the caller can fix, so it is a 422, not a server
/// fault. The message names the cause without internal detail. A scan error
/// also keeps scan's HTTP status, which is not internal, so a scan 403 reads
/// differently from a malformed reply or a dropped connection.
fn execute_failure(e: &anyhow::Error) -> (StatusCode, String) {
    let (status, error) = submission_failure("Failed to execute action", e);
    if e.downcast_ref::<ScanNotConfigured>().is_some() {
        return (StatusCode::UNPROCESSABLE_ENTITY, error);
    }
    if let Some(refused) = e.downcast_ref::<ScanRefused>() {
        let cause = e.to_string();
        let cause = cause.trim_end_matches('.');
        return (
            status,
            format!(
                "Failed to execute action: {cause} (scan answered {}). See the DecMan server log.",
                refused.status
            ),
        );
    }
    (status, error)
}

/// Keeps the outcome of the latest expire attempt on `contract_id`, the
/// evidence that marks a confirmation as one that cannot be archived.
///
/// A success forgets the record. A mediator timeout replaces it with the
/// parties this timeout named, so a timeout that no longer names the
/// confirmer clears its mark. Any other failure, an expire whose outcome is
/// unknown included, says nothing about the confirmer, so the record stays.
///
/// Only expire records. `_Cancel` is controlled by the confirmer, this node's
/// own member, so a cancel timeout never shows that the confirmer is gone.
async fn remember_expire_outcome(
    db: &sqlx::SqlitePool,
    party_id: &CantonId,
    contract_id: &str,
    outcome: &Result<()>,
) {
    let persistence = match outcome {
        Ok(()) => db.clear_mediator_timeout(party_id, contract_id).await,
        Err(error) => match mediator_timeout(error) {
            Some(timeout) => {
                db.record_mediator_timeout(party_id, contract_id, &timeout.unresponsive)
                    .await
            }
            None => return,
        },
    };
    if let Err(e) = persistence {
        tracing::error!(%party_id, contract_id, "Failed to persist the expire outcome: {e}");
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
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 504, description = "The mediator timed out waiting for confirmations; the error names the parties that did not answer", body = ErrorResponse)
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
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let record = WriteRecord {
        step: ActionStep::Expire,
        party_id: party_id.clone(),
        member_party_id: member_party_id.clone(),
        governance_type: body.governance_type,
        action_summary: "expire_confirmation".to_string(),
        details: serde_json::to_string(&*body).unwrap_or_default(),
    };
    let db = data.db.clone();
    let body = body.into_inner();
    let submission = async move {
        let result =
            execute_expire_confirmation(&data.config, &body, &token, &member_party_id, &packages())
                .await;
        remember_expire_outcome(&data.db, &body.party_id, &body.confirmation_cid, &result).await;
        Submitted::of(result, |e| {
            submission_failure("Failed to expire confirmation", e)
        })
    };
    run_write(db, record, "Confirmation expired successfully", submission).await
}

/// Cancel (revoke) own governance confirmation
#[utoipa::path(
    tag = "Governance",
    request_body = CancelConfirmationRequest,
    responses(
        (status = 200, description = "Confirmation cancelled", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 504, description = "The mediator timed out waiting for confirmations; the error names the parties that did not answer", body = ErrorResponse)
    )
)]
#[post("/governance/cancel")]
pub async fn cancel_confirmation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<CancelConfirmationRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party_id = &body.party_id;

    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let record = WriteRecord {
        step: ActionStep::CancelConfirmation,
        party_id: party_id.clone(),
        member_party_id: member_party_id.clone(),
        governance_type: body.governance_type,
        action_summary: "cancel_confirmation".to_string(),
        details: serde_json::to_string(&*body).unwrap_or_default(),
    };
    let db = data.db.clone();
    let body = body.into_inner();
    let submission = async move {
        let result =
            execute_cancel_confirmation(&data.config, &body, &token, &member_party_id, &packages())
                .await;
        Submitted::of(result, |e| {
            submission_failure("Failed to cancel confirmation", e)
        })
    };
    run_write(
        db,
        record,
        "Confirmation cancelled successfully",
        submission,
    )
    .await
}

/// Retract a proposal the caller proposed
#[utoipa::path(
    tag = "Governance",
    request_body = CancelProposalRequest,
    responses(
        (status = 200, description = "Proposal cancelled", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 504, description = "The mediator timed out waiting for confirmations; the error names the parties that did not answer", body = ErrorResponse)
    )
)]
#[post("/governance/cancel-proposal")]
pub async fn cancel_proposal(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<CancelProposalRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party_id = &body.party_id;

    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let record = WriteRecord {
        step: ActionStep::CancelProposal,
        party_id: party_id.clone(),
        member_party_id: member_party_id.clone(),
        governance_type: GovernanceType::CoreDomain,
        action_summary: "cancel_proposal".to_string(),
        details: serde_json::to_string(&*body).unwrap_or_default(),
    };
    let db = data.db.clone();
    let body = body.into_inner();
    let submission = async move {
        let result =
            execute_cancel_proposal(&data.config, &body, &token, &member_party_id, &packages())
                .await;
        Submitted::of(result, |e| {
            submission_failure("Failed to cancel proposal", e)
        })
    };
    run_write(db, record, "Proposal cancelled successfully", submission).await
}

/// Get the decparty's active CouponReassignmentDelegation contracts, newest first
///
/// The vote forms read this so nobody pastes a contract id: setup prefills
/// `prior_delegation`, revoke prefills the contract to archive. Normally zero or
/// one, but the singleton is not ledger-enforced, so several can appear.
#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (
            status = 200,
            description = "Active delegations, newest first; empty when there are none",
            body = ActiveCouponReassignmentDelegation
        ),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Failed to fetch an auth token for party", body = ErrorResponse),
        (status = 503, description = "The delegations could not be read", body = ErrorResponse)
    )
)]
#[get("/coupon-reassignment-delegation")]
pub async fn get_coupon_reassignment_delegation(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let (token, _member) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    match crate::server::reward_automation::active_delegations(
        &data.config,
        &packages(),
        data.test_mode,
        party_id,
        &token,
    )
    .await
    {
        Ok(active) => HttpResponse::Ok().json(ActiveCouponReassignmentDelegation {
            delegations: active.into_iter().map(Into::into).collect(),
        }),
        // Never report a failed read as an empty list. The setup form would
        // prefill a blank `prior_delegation` and hit the propose-time 409 after
        // the whole split is filled in, and revoke would look unavailable.
        Err(e) => {
            tracing::warn!(%party_id, error = %e, "active delegation read failed");
            HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: format!("cannot read this party's current delegations: {e}"),
            })
        }
    }
}

/// Get the decparty's reward automation status on this node
///
/// Says whether this participant resolves the rewards DAR, which delegation is
/// active, and whether it lists this node's member party as an assigner. A
/// missing DAR is a 200 with `rewards_dar_resolvable: false`, not an error.
#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (
            status = 200,
            description = "The reward status; `rewards_dar_resolvable` is false when this participant lacks the rewards DAR",
            body = RewardStatusResponse
        ),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Failed to fetch an auth token for party", body = ErrorResponse),
        (status = 503, description = "The reward status could not be read", body = ErrorResponse)
    )
)]
#[get("/reward-status")]
pub async fn get_reward_status(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let (token, member) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    match crate::server::reward_automation::reward_status(
        &data.config,
        &packages(),
        data.test_mode,
        party_id,
        &member,
        &token,
    )
    .await
    {
        Ok(status) => HttpResponse::Ok().json(status),
        Err(e) => {
            tracing::warn!(%party_id, error = %e, "reward status read failed");
            HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: format!("cannot read this party's reward status: {e}"),
            })
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get token for a party from auth registry
pub(crate) async fn get_party_token(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<String> {
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

    let channel = data.config.admin_channel().await.ok()?;

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
                    client_version: None,
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

/// Get token and member_party_id for a party.
///
/// `Ok(None)`: no auth configured, or the party is unknown to the registry.
/// `Err`: the party is known, but the token fetch failed (logged here).
pub(crate) async fn get_party_credentials(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Result<Option<(String, CantonId)>> {
    let auth = data.auth.read().await;
    match &*auth {
        Some(WorkflowAuth::Keycloak(registry)) => {
            let Some(tm) = registry.get(party_id) else {
                return Ok(None);
            };
            match tm.get_token().await {
                Ok(token) => Ok(Some((token, tm.member_party_id().clone()))),
                Err(e) => {
                    tracing::warn!(%party_id, error = %e, "failed to fetch auth token for party");
                    Err(e.into())
                }
            }
        }
        Some(WorkflowAuth::Mock(mock_registry)) => {
            let mm = mock_registry.get(party_id).await;
            Ok(Some((mm.get_token(), mm.member_party_id().clone())))
        }
        None => Ok(None),
    }
}

/// Get the hardcoded default package config (package IDs are constants, not per-party)
pub(crate) fn packages() -> PackageConfig {
    default_package_config()
}

// ============================================================================
// Ledger Command Execution
// ============================================================================

/// Execute ConfirmAction choice on the GovernanceRules contract with structured action
async fn execute_confirm_action(
    config: &NodeConfig,
    request: &ConfirmActionRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut rules = governance_rules_template(packages)?;
            // The rules contract may be an out-of-date fallback living under an
            // older governance-core package — exercise it under its actual
            // package ref.
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_confirm_self_action(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.action,
                command_id,
            )?
        }
        GovernanceType::CoreDomain => {
            let mut rules = governance_rules_template(packages)?;
            let proposal_cid = request
                .proposal_cid
                .as_deref()
                .context("proposal_cid required for core_domain confirm")?;
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_confirm_proposal(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                proposal_cid,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let request = ledger_request(
        SubmitAndWaitRequest {
            commands: Some(commands),
        },
        token,
        write_deadline(),
    )?;
    client.submit_and_wait(request).await?;

    Ok(())
}

/// Execute ExecuteConfirmedAction choice on governance rules contract
async fn execute_confirmed_action(
    config: &NodeConfig,
    http_client: &reqwest::Client,
    request: &ExecuteActionRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    // The executor's submission must carry the contracts the proposal's
    // execute step reads but no member can see. `AcceptTransferProposal` needs
    // the registry-supplied transfer rule and its dependencies;
    // `RequestDevNetFeaturedAppRight` needs the DSO's `AmuletRules`;
    // `AcceptPaidCredential` needs `AmuletRules`, its open mining round and any
    // featured app right it names. All key on the on-chain proposal's
    // template, not on `governance_type`, so a non-CoreDomain path carrying
    // such a proposal still gets its contracts.
    let mut registry_disclosed: Vec<DisclosedContract> = Vec::new();
    let mut proposal_event: Option<CreatedEvent> = None;
    if let Some(proposal_cid) = request.proposal_cid.as_deref() {
        match fetch_proposal_created_event(
            config,
            Some(token.to_string()),
            &request.party_id,
            proposal_cid,
        )
        .await
        {
            Ok(event) => proposal_event = event,
            // Don't hard-fail on ledger hiccups here; the submission below
            // surfaces a clear error if the proposal really is gone.
            Err(e) => tracing::warn!("Failed to look up proposal {proposal_cid}: {e:#}"),
        }
    }
    if let (Some(proposal_cid), Some(created)) =
        (request.proposal_cid.as_deref(), proposal_event.as_ref())
    {
        match maybe_fetch_for_proposal_event(
            config,
            Some(token.to_string()),
            &request.party_id,
            created,
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

        if has_template(
            created,
            RequestDevNetFeaturedAppRight::MODULE,
            RequestDevNetFeaturedAppRight::ENTITY,
        ) {
            let proposed_cid = created
                .create_arguments
                .as_ref()
                .and_then(|args| record_field(args, "amuletRulesCid"))
                .and_then(|v| match v {
                    value::Sum::ContractId(cid) => Some(cid.as_str()),
                    _ => None,
                });
            let current = fetch_amulet_rules(http_client, config)
                .await
                .context("Failed to fetch AmuletRules from the DSO API")?;
            registry_disclosed.push(amulet_rules_disclosure(proposed_cid, &current)?);
        }

        if has_template(
            created,
            AcceptPaidCredential::MODULE,
            AcceptPaidCredential::ENTITY,
        ) {
            let pins = PaidAcceptPins::of(created)?;
            let requested: HashSet<&str> = request
                .disclosed_contracts
                .iter()
                .map(|dc| dc.contract_id.as_str())
                .collect();
            registry_disclosed
                .extend(paid_accept_disclosures(http_client, config, &pins, &requested).await?);
        }
    }

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

    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut rules = governance_rules_template(packages)?;
            // The rules contract may be an out-of-date fallback living under an
            // older governance-core package — exercise it under its actual
            // package ref.
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_execute_self_action(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.action,
                &request.confirmation_cids,
                disclosed_contracts,
                command_id,
            )?
        }
        GovernanceType::CoreDomain => {
            let mut rules = governance_rules_template(packages)?;
            let proposal_cid = request
                .proposal_cid
                .as_deref()
                .context("proposal_cid required for core_domain execute")?;
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_execute_proposal(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                proposal_cid,
                &request.confirmation_cids,
                disclosed_contracts,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let request = ledger_request(
        SubmitAndWaitRequest {
            commands: Some(commands),
        },
        token,
        write_deadline(),
    )?;
    client.submit_and_wait(request).await?;

    Ok(())
}

/// The `AmuletRules` disclosure for a `RequestDevNetFeaturedAppRight` or
/// `AcceptPaidCredential` execute.
///
/// Only the DSO can see `AmuletRules`, so the executor discloses the current
/// contract from the DSO scan API. The proposal pins a contract id. When the
/// DSO has replaced its `AmuletRules` since, the pinned contract is archived
/// and the execute can never succeed, so report that instead of disclosing a
/// contract the choice will not read.
fn amulet_rules_disclosure(
    proposed_cid: Option<&str>,
    current: &AmuletRulesContract,
) -> Result<DisclosedContract> {
    let proposed_cid = proposed_cid.context("The proposal has no amuletRulesCid")?;
    if proposed_cid != current.contract_id {
        anyhow::bail!(
            "The proposal names AmuletRules {proposed_cid}, but the DSO's current AmuletRules is {}. Cancel this proposal and propose again.",
            current.contract_id
        );
    }
    disclosure(
        &current.contract_id,
        &current.created_event_blob,
        "the DSO's AmuletRules",
    )
}

/// A disclosed contract from a base64 created-event blob. `what` names the
/// contract in the error.
fn disclosure(contract_id: &str, blob: &str, what: &str) -> Result<DisclosedContract> {
    Ok(DisclosedContract {
        template_id: None,
        contract_id: contract_id.to_owned(),
        created_event_blob: base64::engine::general_purpose::STANDARD
            .decode(blob)
            .with_context(|| format!("Invalid base64 in the created_event_blob of {what}"))?,
        synchronizer_id: String::new(),
    })
}

/// The DSO contracts an `AcceptPaidCredential` proposal pins in its
/// `appTransferContext`.
#[derive(Debug)]
struct PaidAcceptPins {
    amulet_rules: String,
    open_mining_round: String,
    featured_app_right: Option<String>,
}

impl PaidAcceptPins {
    fn of(created: &CreatedEvent) -> Result<Self> {
        let context = created
            .create_arguments
            .as_ref()
            .and_then(|args| field_record(args, "appTransferContext"))
            .context("The AcceptPaidCredential proposal has no appTransferContext")?;
        let featured_app_right = match record_field(context, "featuredAppRight") {
            Some(value::Sum::Optional(optional)) => optional
                .value
                .as_deref()
                .map(extract_contract_id)
                .transpose()
                .context("The AcceptPaidCredential featuredAppRight is not a contract id")?,
            _ => None,
        };
        Ok(Self {
            amulet_rules: field_contract_id(context, "amuletRules")
                .context("The AcceptPaidCredential proposal has no amuletRules")?,
            open_mining_round: field_contract_id(context, "openMiningRound")
                .context("The AcceptPaidCredential proposal has no openMiningRound")?,
            featured_app_right,
        })
    }
}

/// The disclosures an `AcceptPaidCredential` execute needs, minus those the
/// request already carries. The governance party sees none of the three
/// contracts. `AmuletRules` comes from the DSO API, the rest from scan.
///
/// # Errors
/// Returns [`ScanNotConfigured`] when scan must be read and `DECPM_SCAN_URL`
/// is not set. Returns an error when a pinned contract is no longer active.
async fn paid_accept_disclosures(
    http_client: &reqwest::Client,
    config: &NodeConfig,
    pins: &PaidAcceptPins,
    requested: &HashSet<&str>,
) -> Result<Vec<DisclosedContract>> {
    let needs_round = !requested.contains(pins.open_mining_round.as_str());
    let featured_app_right = pins
        .featured_app_right
        .as_deref()
        .filter(|cid| !requested.contains(cid));
    let scan_url = if needs_round || featured_app_right.is_some() {
        Some(config.scan_url().ok_or(ScanNotConfigured)?)
    } else {
        None
    };

    let mut disclosed = Vec::new();
    if !requested.contains(pins.amulet_rules.as_str()) {
        let current = fetch_amulet_rules(http_client, config)
            .await
            .context("Failed to fetch AmuletRules from the DSO API")?;
        disclosed.push(amulet_rules_disclosure(Some(&pins.amulet_rules), &current)?);
    }
    let Some(scan_url) = scan_url else {
        return Ok(disclosed);
    };
    if needs_round {
        let rounds = scan::fetch_open_mining_rounds(http_client, scan_url)
            .await
            .context("Failed to fetch the open mining rounds from scan")?;
        disclosed.push(open_round_disclosure(&pins.open_mining_round, &rounds)?);
    }
    if let Some(cid) = featured_app_right {
        let right = scan::fetch_featured_app_right(http_client, scan_url, cid)
            .await
            .context("Failed to fetch the FeaturedAppRight from scan")?
            .with_context(|| {
                format!(
                    "The proposal names FeaturedAppRight {cid}, which scan does not list as active. Cancel this proposal and propose again."
                )
            })?;
        disclosed.push(disclosure(
            &right.contract_id,
            &right.created_event_blob,
            "the FeaturedAppRight",
        )?);
    }
    Ok(disclosed)
}

/// The disclosure of the open round a paid accept pins. Scan lists a round
/// until the DSO archives it. A pinned round missing from the list has
/// closed, and the accept can never execute.
fn open_round_disclosure(pinned: &str, rounds: &[OpenMiningRound]) -> Result<DisclosedContract> {
    let round = rounds
        .iter()
        .find(|r| r.contract.contract_id == pinned)
        .with_context(|| {
            format!(
                "The proposal pins open mining round {pinned}, which has closed. Cancel this proposal and propose again."
            )
        })?;
    disclosure(
        &round.contract.contract_id,
        &round.contract.created_event_blob,
        "the open mining round",
    )
}

/// Execute ExpireConfirmation choice on governance rules contract
async fn execute_expire_confirmation(
    config: &NodeConfig,
    request: &ExpireConfirmationRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    // All three flows take the same argument shape ({ member,
    // staleConfirmationCid }); only the template and the choice differ.
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut rules = governance_rules_template(packages)?;
            // The rules contract may be an out-of-date fallback living under an
            // older governance-core package — exercise it under its actual
            // package ref.
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_expire_self_confirmation(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.confirmation_cid,
                command_id,
            )
        }
        GovernanceType::CoreDomain => {
            // Same `GovernanceRules` template as CoreSelf but a different choice:
            // `GovernanceRules_ExpireConfirmation` operates on the
            // `GovernanceConfirmation` template (domain action confirmations)
            // rather than `GovernanceSelfConfirmation`.
            let mut rules = governance_rules_template(packages)?;
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_expire_domain_confirmation(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.confirmation_cid,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let request = ledger_request(
        SubmitAndWaitRequest {
            commands: Some(commands),
        },
        token,
        write_deadline(),
    )?;
    client.submit_and_wait(request).await?;

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
    // Every `_Cancel` choice is controller=confirmer and takes no arguments;
    // only the confirmation template differs.
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut confirmation = self_confirmation_template(packages)?;
            // The confirmation contract is created by the rules contract's
            // choice, so it shares the rules contract's (possibly out-of-date)
            // package — exercise it under its actual package ref.
            confirmation.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.confirmation_cid,
                &confirmation.package_ref,
            )
            .await;
            build_cancel_self_confirmation(
                &confirmation,
                &request.confirmation_cid,
                member_party_id,
                &request.party_id,
                command_id,
            )
        }
        GovernanceType::CoreDomain => {
            // Domain confirmations live in their own template
            // `GovernanceConfirmation` (module `Governance.Confirmation`).
            let mut confirmation = domain_confirmation_template(packages)?;
            confirmation.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.confirmation_cid,
                &confirmation.package_ref,
            )
            .await;
            build_cancel_domain_confirmation(
                &confirmation,
                &request.confirmation_cid,
                member_party_id,
                &request.party_id,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let request = ledger_request(
        SubmitAndWaitRequest {
            commands: Some(commands),
        },
        token,
        write_deadline(),
    )?;
    client.submit_and_wait(request).await?;

    Ok(())
}

/// Exercise `GovernableAction_ProposerCancel` on a proposal, and archive the
/// caller's own confirmation alongside it.
///
/// The two commands share one submission so they succeed or fail together. A
/// confirmation that outlives its proposal is only clearable by its own
/// confirmer: `GovernanceConfirmation_Expire` refuses to run before the
/// confirmation's expiry time.
async fn execute_cancel_proposal(
    config: &NodeConfig,
    request: &CancelProposalRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    // `GovernableAction_ProposerCancel` is declared on the interface rather
    // than on any one template, so the exercise carries the `GovernableAction`
    // interface id and never the id of the template that actually created the
    // contract. That also rules out `resolve_contract_package_ref` here: it
    // reports the package of the contract, which is the proposal's own
    // package, not the interface's.
    let interface = governable_action_interface(packages)?;

    let own_confirmation = match request.confirmation_cid.as_deref() {
        Some(cid) => {
            let mut confirmation = domain_confirmation_template(packages)?;
            // The confirmation is created by the rules contract's choice, so it
            // shares that contract's possibly out-of-date package.
            confirmation.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                cid,
                &confirmation.package_ref,
            )
            .await;
            Some(confirmation)
        }
        None => None,
    };

    let commands = build_cancel_proposal(
        &interface,
        &request.proposal_cid,
        request
            .confirmation_cid
            .as_deref()
            .zip(own_confirmation.as_ref()),
        member_party_id,
        &request.party_id,
        uuid::Uuid::new_v4().to_string(),
    );

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let request = ledger_request(
        SubmitAndWaitRequest {
            commands: Some(commands),
        },
        token,
        write_deadline(),
    )?;
    client.submit_and_wait(request).await?;

    Ok(())
}

#[cfg(test)]
mod propose_guard_tests {
    use decman_lib::catalog::proposals::core::GenericVote;

    use super::*;

    fn party() -> CantonId {
        // `prefix::<68 hex chars>` = a 34-byte all-zero namespace: a well-formed id.
        CantonId::parse(&format!("p::{}", "0".repeat(68))).expect("valid canton id")
    }

    /// Only a Setup that omits `prior_delegation` can conflict with an active
    /// delegation. Replacing one is the normal path and must stay allowed, and no
    /// other proposal type is affected — a rule broad enough to block either would
    /// make the reward action unusable.
    ///
    /// This also gates the ACS read: a shape that returns false here never
    /// touches the ledger, so a false positive would tax every propose.
    #[test]
    fn only_an_unnamed_replacement_can_create_a_second_delegation() {
        let setup_without_prior =
            ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
                dso: party(),
                assigners: vec![party()],
                new_beneficiaries: vec![],
                prior_delegation: None,
            });
        let setup_with_prior =
            ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
                dso: party(),
                assigners: vec![party()],
                new_beneficiaries: vec![],
                prior_delegation: Some("00abc".to_string()),
            });
        let unrelated = ProposalType::GenericVote(GenericVote {
            description: "unrelated".to_string(),
        });

        // the accident: creating one without naming what it replaces
        assert!(may_create_second_delegation(&setup_without_prior));
        // replacement names what it replaces -> always allowed, no read needed
        assert!(!may_create_second_delegation(&setup_with_prior));
        // unrelated proposals are never blocked, and never pay for the read
        assert!(!may_create_second_delegation(&unrelated));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withholding_drops_the_payload_and_keeps_the_summary() -> anyhow::Result<()> {
        let mut actions: Vec<DomainGovernanceAction> =
            vec![serde_json::from_value(serde_json::json!({
                "proposal_cid": "00proposal",
                "action_label": "GenericVote",
                "description": "vote on it",
                "confirmations": [],
                "confirmation_count": 0,
                "executable_confirmation_cids": [],
                "can_execute": false,
                "proposal_template_id": "pkg:Governance.GenericVote:GenericVoteProposal",
                "proposal_payload": {"description": "vote on it", "secret": "x"},
            }))?];

        withhold_payloads(&mut actions);

        let wire = serde_json::to_value(&actions[0])?;
        assert!(wire.get("proposal_payload").is_none(), "{wire}");
        assert!(wire.get("proposal_template_id").is_none(), "{wire}");
        assert_eq!(wire["description"], "vote on it");
        assert_eq!(wire["proposal_cid"], "00proposal");
        Ok(())
    }

    fn amulet_rules(contract_id: &str, created_event_blob: &str) -> AmuletRulesContract {
        AmuletRulesContract {
            dso: CantonId::parse(
                "dso::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
            )
            .expect("valid canton id"),
            contract_id: contract_id.to_owned(),
            created_event_blob: created_event_blob.to_owned(),
        }
    }

    /// "blob" in base64.
    const BLOB_B64: &str = "YmxvYg==";

    #[test]
    fn amulet_rules_disclosure_discloses_the_current_contract_when_the_proposal_matches() {
        let current = amulet_rules("00amulet", BLOB_B64);
        let disclosed = amulet_rules_disclosure(Some("00amulet"), &current)
            .expect("a matching cid is disclosed");
        assert_eq!(disclosed.contract_id, "00amulet");
        assert_eq!(disclosed.created_event_blob, b"blob");
        assert!(disclosed.template_id.is_none());
    }

    #[test]
    fn amulet_rules_disclosure_names_both_cids_when_the_dso_replaced_amulet_rules() {
        let current = amulet_rules("00new", BLOB_B64);
        let err = amulet_rules_disclosure(Some("00old"), &current)
            .expect_err("a stale proposal cannot be executed");
        let message = format!("{err:#}");
        assert!(message.contains("00old"), "{message}");
        assert!(message.contains("00new"), "{message}");
    }

    #[test]
    fn amulet_rules_disclosure_rejects_a_proposal_without_amulet_rules_cid() {
        let current = amulet_rules("00amulet", BLOB_B64);
        assert!(amulet_rules_disclosure(None, &current).is_err());
    }

    #[test]
    fn amulet_rules_disclosure_rejects_a_malformed_blob() {
        let current = amulet_rules("00amulet", "not base64!");
        assert!(amulet_rules_disclosure(Some("00amulet"), &current).is_err());
    }

    /// `core_domain` builds its choice from `proposal_cid` and never reads
    /// `action`, so both clients send `governance_set_threshold: 0` as a
    /// deliberate placeholder (`NotificationsView.tsx`, `app.rs`). The inline
    /// guard must let it through untouched — validating it would 400 every
    /// domain confirm and execute.
    #[test]
    fn domain_confirms_accept_the_zero_threshold_placeholder() {
        let placeholder = ActionType::GovernanceSetThreshold { new_threshold: 0 };
        assert!(
            validate_inline_action(&placeholder, GovernanceType::CoreDomain).is_ok(),
            "the core_domain placeholder must not be validated"
        );
    }

    /// The same placeholder on the inline path is a real threshold, and 0 is
    /// not a legal one.
    #[test]
    fn self_management_confirms_still_validate_the_threshold() {
        let bad = ActionType::GovernanceSetThreshold { new_threshold: 0 };
        match validate_inline_action(&bad, GovernanceType::CoreSelf) {
            Ok(()) => panic!("new_threshold 0 must not pass core_self validation"),
            Err(error) => assert!(
                error.contains("at least 1"),
                "error should name the bound: {error}"
            ),
        }
    }

    /// A non-self-management action paired with `core_self` is a client error,
    /// caught before it reaches the serializer that cannot encode it.
    #[test]
    fn self_management_confirms_reject_a_domain_action() {
        let action = ActionType::DevNetFeatureApp {
            amulet_rules_cid: "00amulet".to_owned(),
        };
        match validate_inline_action(&action, GovernanceType::CoreSelf) {
            Ok(()) => panic!("DevNetFeatureApp is not a self-management action"),
            Err(error) => assert!(
                error.contains("/governance/propose"),
                "error should point at the proposal path: {error}"
            ),
        }
    }

    /// A cancellation vote is a self-management action, so `core_self` takes
    /// it, but only when it names the proposal to cancel.
    #[test]
    fn self_management_confirms_take_a_cancellation_with_a_proposal_cid() {
        let cancel = |proposal_cid: &str| ActionType::GovernanceCancelAction {
            proposal_cid: proposal_cid.to_owned(),
        };
        assert!(validate_inline_action(&cancel("00proposal"), GovernanceType::CoreSelf).is_ok());
        match validate_inline_action(&cancel(""), GovernanceType::CoreSelf) {
            Ok(()) => panic!("a cancellation without a proposal cid must not pass"),
            Err(error) => assert!(
                error.contains("proposal_cid"),
                "error should name the field: {error}"
            ),
        }
    }

    fn entry_at(offset: i64) -> ChainAuditEntry {
        ChainAuditEntry {
            offset,
            timestamp: 0,
            event_type: "propose".to_string(),
            contract_id: String::new(),
            template_id: String::new(),
            package_id: String::new(),
            governance_type: "core_domain".to_string(),
            action_summary: String::new(),
            choice: None,
            acting_parties: Vec::new(),
            update_id: String::new(),
            details: serde_json::Value::Null,
        }
    }

    fn cache_row_at(offset: i64) -> ChainAuditCacheRow {
        ChainAuditCacheRow {
            party_id: "dec::1220ff".to_string(),
            offset,
            timestamp: 0,
            event_type: "propose".to_string(),
            contract_id: String::new(),
            template_id: String::new(),
            package_id: String::new(),
            governance_type: "core_domain".to_string(),
            action_summary: String::new(),
            choice: None,
            acting_parties: "[]".to_string(),
            update_id: String::new(),
            details: "null".to_string(),
        }
    }

    /// The cursor is the oldest offset on the page, since the next request asks
    /// for `offset < cursor`.
    #[test]
    fn cursor_is_the_oldest_offset_when_more_exist() {
        let response = chain_audit_response(vec![entry_at(30), entry_at(20), entry_at(10)], true);
        assert_eq!(response.next_before_offset, Some(10));
        assert_eq!(response.total_returned, 3);
    }

    #[test]
    fn no_cursor_when_nothing_older_exists() {
        let response = chain_audit_response(vec![entry_at(30), entry_at(20)], false);
        assert_eq!(response.next_before_offset, None);
        assert_eq!(response.total_returned, 2);
    }

    /// A `has_more` with no rows to derive a cursor from must not hand out one:
    /// there is no offset to page before, and inventing one would skip entries.
    #[test]
    fn no_cursor_from_an_empty_page() {
        let response = chain_audit_response(Vec::new(), true);
        assert_eq!(response.next_before_offset, None);
        assert_eq!(response.total_returned, 0);
    }

    /// An empty cache read is a miss, not "the trail ends here" — the caller
    /// has to fall through to Canton.
    #[test]
    fn empty_cache_is_a_miss() {
        assert!(cached_chain_audit_page(Vec::new(), 2).is_none());
    }

    /// A cache page that fills `limit` is reported as having more, so the next
    /// request reaches Canton rather than stopping at the cache's tail.
    #[test]
    fn full_cache_page_reports_more() {
        let response = cached_chain_audit_page(vec![cache_row_at(30), cache_row_at(20)], 2)
            .expect("non-empty rows are a hit");
        assert_eq!(response.next_before_offset, Some(20));
    }

    #[test]
    fn short_cache_page_reports_no_more() {
        let response =
            cached_chain_audit_page(vec![cache_row_at(30)], 2).expect("non-empty rows are a hit");
        assert_eq!(response.next_before_offset, None);
        assert_eq!(response.total_returned, 1);
    }

    /// An over-`limit` cache page (a whole offset group kept together) still
    /// reports more, and its cursor is the oldest offset returned.
    #[test]
    fn oversized_cache_page_reports_more() {
        let rows = vec![cache_row_at(30), cache_row_at(20), cache_row_at(20)];
        let response = cached_chain_audit_page(rows, 2).expect("non-empty rows are a hit");
        assert_eq!(response.total_returned, 3);
        assert_eq!(response.next_before_offset, Some(20));
    }

    /// Each nonce is 12 lowercase hex digits, which the template's ensure
    /// clause accepts (non-empty, no "/"), and no two draws repeat.
    #[test]
    fn credential_id_nonces_are_distinct_lowercase_hex() {
        let nonces: HashSet<String> = (0..1000).map(|_| credential_id_nonce()).collect();
        assert_eq!(nonces.len(), 1000, "a nonce repeated");
        for nonce in &nonces {
            assert_eq!(nonce.len(), 12, "{nonce}");
            assert!(
                nonce
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{nonce}"
            );
        }
    }
}

#[cfg(test)]
mod governance_audit_endpoint_tests {
    use actix_web::{App, http::StatusCode, test as actix_test};
    use serde_json::Value;
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::MIGRATOR;

    fn party(prefix: &str) -> Result<CantonId> {
        CantonId::parse(&format!("{prefix}::1220{}", "ab".repeat(32)))
    }

    async fn log_row(
        pool: &SqlitePool,
        party_id: &CantonId,
        event_type: &str,
        created_at: i64,
    ) -> Result {
        sqlx::query(
            r"
            INSERT INTO governance_audit (
                timestamp, event_type, party_id, member_party_id,
                governance_type, action_summary, details, status,
                error_message, created_at
            ) VALUES (?, ?, ?, ?, 'core_domain', 'transfer', '{}', 'success', NULL, ?)
            ",
        )
        .bind(created_at)
        .bind(event_type)
        .bind(party_id.to_string())
        .bind(party("member")?.to_string())
        .bind(created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// `(party_id, created_at)` of every entry the endpoint returns for `uri`.
    async fn fetch(pool: &SqlitePool, uri: &str) -> Result<Vec<(String, i64)>> {
        let app = actix_test::init_service(
            App::new()
                .app_data(AppState::for_test_on(None, pool.clone()))
                .service(get_governance_audit),
        )
        .await;
        let response =
            actix_test::call_service(&app, actix_test::TestRequest::get().uri(uri).to_request())
                .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = actix_test::read_body_json(response).await;
        let entries = body["entries"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("no entries array in {body}"))?;
        assert_eq!(body["total_returned"].as_u64(), Some(entries.len() as u64));
        entries
            .iter()
            .map(|e| {
                let party_id = e["party_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("entry without party_id: {e}"))?;
                let created_at = e["created_at"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("entry without created_at: {e}"))?;
                Ok((party_id.to_string(), created_at))
            })
            .collect()
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn without_a_party_it_lists_every_party_newest_first(pool: SqlitePool) -> Result {
        let (a, b) = (party("party-a")?, party("party-b")?);
        log_row(&pool, &a, "propose", 1000).await?;
        log_row(&pool, &b, "confirm", 2000).await?;
        log_row(&pool, &a, "execute", 3000).await?;

        let entries = fetch(&pool, "/governance/audit").await?;

        assert_eq!(
            entries,
            vec![
                (a.to_string(), 3000),
                (b.to_string(), 2000),
                (a.to_string(), 1000),
            ]
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn with_a_party_it_lists_only_that_party(pool: SqlitePool) -> Result {
        let (a, b) = (party("party-a")?, party("party-b")?);
        log_row(&pool, &a, "propose", 1000).await?;
        log_row(&pool, &b, "confirm", 2000).await?;
        log_row(&pool, &a, "execute", 3000).await?;

        let uri = format!("/governance/audit?party_id={a}&limit=1&offset=1");
        let entries = fetch(&pool, &uri).await?;

        assert_eq!(entries, vec![(a.to_string(), 1000)]);
        Ok(())
    }

    /// SQLite reads a negative LIMIT as "no limit", so an unscoped read must
    /// clamp the value rather than pass it through.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn without_a_party_the_limit_is_bounded(pool: SqlitePool) -> Result {
        let a = party("party-a")?;
        for created_at in 0..=MAX_RECENT_AUDIT_LIMIT {
            log_row(&pool, &a, "confirm", created_at).await?;
        }

        let huge = fetch(&pool, "/governance/audit?limit=100000").await?;
        assert_eq!(huge.len() as i64, MAX_RECENT_AUDIT_LIMIT);
        assert_eq!(huge[0].1, MAX_RECENT_AUDIT_LIMIT);

        let negative = fetch(&pool, "/governance/audit?limit=-1&offset=-5").await?;
        assert_eq!(negative, vec![(a.to_string(), MAX_RECENT_AUDIT_LIMIT)]);
        Ok(())
    }
}

#[cfg(test)]
mod get_party_credentials_tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use tracing_subscriber::fmt::MakeWriter;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path_regex},
    };

    use super::*;
    use crate::{
        auth::{AuthRegistry, WorkflowAuth},
        config::{KeycloakConfig, PartyCredentials},
    };

    #[derive(Clone, Default)]
    pub(super) struct LogBuffer(Arc<StdMutex<Vec<u8>>>);

    impl std::io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut written = self
                .0
                .lock()
                .map_err(|_| std::io::Error::other("log buffer lock poisoned"))?;
            written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for LogBuffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl LogBuffer {
        pub(super) fn captured(&self) -> Result<String> {
            let written = self
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("log buffer lock poisoned"))?
                .clone();
            Ok(String::from_utf8(written)?)
        }
    }

    fn party_id() -> Result<CantonId> {
        CantonId::parse(&format!("p::{}", "0".repeat(68)))
    }

    #[tokio::test]
    async fn no_auth_configured_is_ok_none() -> Result<()> {
        let data = AppState::for_test(None).await?;
        assert!(get_party_credentials(&data, &party_id()?).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn unknown_party_is_ok_none() -> Result<()> {
        let registry = AuthRegistry::new(&[]).await?;
        let data = AppState::for_test(Some(WorkflowAuth::Keycloak(Arc::new(registry)))).await?;
        assert!(get_party_credentials(&data, &party_id()?).await?.is_none());
        Ok(())
    }

    /// The party authenticates fine at startup (`expires_in: 0` puts the token
    /// on the refresh path immediately), then the Keycloak stand-in fails the
    /// very next request — the one `get_party_credentials` makes.
    #[tokio::test]
    async fn token_fetch_failure_is_err_and_logged() -> Result<()> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token": "t", "expires_in": 0})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(serde_json::json!({"error": "down"})),
            )
            .mount(&server)
            .await;

        let id = party_id()?;
        let credentials = PartyCredentials {
            dec_party_id: id.clone(),
            member_party_id: id.clone(),
            user_id: "user".to_string(),
            keycloak: KeycloakConfig {
                url: server.uri(),
                realm: "test-realm".to_string(),
                client_id: "decman".to_string(),
                client_secret: Some("secret".to_string()),
                ..KeycloakConfig::default()
            },
            auth0: None,
            packages: PackageConfig::default(),
        };
        let registry = AuthRegistry::new(&[credentials]).await?;
        let data = AppState::for_test(Some(WorkflowAuth::Keycloak(Arc::new(registry)))).await?;

        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();
        let result = {
            let _guard = tracing::subscriber::set_default(subscriber);
            get_party_credentials(&data, &id).await
        };

        assert!(
            result.is_err(),
            "a failed token fetch must be an error, not a silent None"
        );
        let logs = buffer.captured()?;
        assert!(logs.contains("WARN"), "no warning was logged: {logs}");
        assert!(
            logs.contains(&id.to_string()),
            "the log omits the party id: {logs}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod action_metrics_tests {
    use std::sync::Arc;

    use actix_web::{
        App,
        http::{StatusCode, header::AUTHORIZATION},
        test as actix_test,
    };

    use super::*;
    use crate::{auth::MockAuthRegistry, server::middleware::AuthMiddleware};

    fn party_id() -> Result<CantonId> {
        CantonId::parse(&format!("p::{}", "0".repeat(68)))
    }

    fn count(step: ActionStep, outcome: ActionOutcome) -> u64 {
        ACTIONS
            .with_label_values(&[step.label(), outcome.label()])
            .get()
    }

    /// Mock credentials and a Ledger API on port 1, where nothing listens, so
    /// every submission reaches the ledger path and fails there.
    async fn state_with_ledger_down() -> Result<web::Data<AppState>> {
        let mut state = AppState::for_test(None).await?.into_inner();
        Arc::get_mut(&mut state)
            .context("a fresh test state has one owner")?
            .config
            .canton
            .ledger_api_port = 1;
        let mock = MockAuthRegistry::new(state.party_credentials.clone());
        *state.auth.write().await = Some(WorkflowAuth::Mock(Arc::new(mock)));
        Ok(web::Data::from(state))
    }

    #[test]
    fn labels_are_the_strings_dashboards_query() {
        assert_eq!(
            ActionStep::ALL.map(ActionStep::label),
            [
                "propose",
                "confirm",
                "execute",
                "expire",
                "cancel_confirmation",
                "cancel_proposal"
            ]
        );
        assert_eq!(
            ActionOutcome::ALL.map(ActionOutcome::label),
            ["ok", "error"]
        );
    }

    #[test]
    fn every_step_and_outcome_is_exported_before_its_first_submission() -> Result<()> {
        register_metrics();
        let family = prometheus::gather()
            .into_iter()
            .find(|f| f.name() == "decman_governance_actions_total")
            .context("the family is not registered")?;
        let series: Vec<Vec<&str>> = family
            .get_metric()
            .iter()
            .map(|m| m.get_label().iter().map(|l| l.value()).collect())
            .collect();
        for step in ActionStep::ALL {
            for outcome in ActionOutcome::ALL {
                assert!(
                    series.iter().any(|labels| labels.contains(&step.label())
                        && labels.contains(&outcome.label())),
                    "no series for {step:?}/{outcome:?}"
                );
            }
        }
        Ok(())
    }

    // Every test that posts to a write handler moves the same series, so they
    // take turns: every test in this binary shares one registry and runs in
    // parallel.
    pub(super) static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Posts `body` as an admin through all six governance write handlers.
    async fn post(uri: &str, body: serde_json::Value) -> Result<StatusCode> {
        let data = state_with_ledger_down().await?;
        // The middleware attaches the principal that `require_admin` checks.
        let app = actix_test::init_service(
            App::new()
                .app_data(data)
                .wrap(AuthMiddleware)
                .service(propose_action)
                .service(confirm_action)
                .service(execute_action)
                .service(expire_confirmation)
                .service(cancel_confirmation)
                .service(cancel_proposal),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri(uri)
            .insert_header((AUTHORIZATION, "Bearer any-token"))
            .set_json(body)
            .to_request();
        Ok(actix_test::call_service(&app, request).await.status())
    }

    /// How far a step's `error` and `ok` series moved while `uri` was posted.
    async fn moved(step: ActionStep, uri: &str, body: serde_json::Value) -> Result<(u64, u64)> {
        let error_before = count(step, ActionOutcome::Error);
        let ok_before = count(step, ActionOutcome::Ok);
        let status = post(uri, body).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{step:?}");
        Ok((
            count(step, ActionOutcome::Error) - error_before,
            count(step, ActionOutcome::Ok) - ok_before,
        ))
    }

    #[actix_web::test]
    async fn every_step_counts_an_attempt_the_ledger_never_answered_as_one_error() -> Result<()> {
        let _serial = SERIAL.lock().await;
        let party = party_id()?.to_string();
        let action = serde_json::json!({ "type": "governance_set_threshold", "new_threshold": 2 });
        let cases = [
            (
                ActionStep::Propose,
                "/governance/propose",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "proposal": { "type": "generic_vote", "description": "metrics" },
                }),
            ),
            (
                ActionStep::Confirm,
                "/governance/confirm",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": action,
                }),
            ),
            (
                ActionStep::Execute,
                "/governance/execute",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": action,
                    "confirmation_cids": ["00c0ffee"],
                }),
            ),
            (
                ActionStep::Expire,
                "/governance/expire",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "confirmation_cid": "00c0ffee",
                }),
            ),
            (
                ActionStep::CancelConfirmation,
                "/governance/cancel",
                serde_json::json!({ "party_id": party, "confirmation_cid": "00c0ffee" }),
            ),
            (
                ActionStep::CancelProposal,
                "/governance/cancel-proposal",
                serde_json::json!({ "party_id": party, "proposal_cid": "00p" }),
            ),
        ];
        for (step, uri, body) in cases {
            assert_eq!(moved(step, uri, body).await?, (1, 0), "{step:?}");
        }
        Ok(())
    }

    #[actix_web::test]
    async fn a_request_refused_before_submission_does_not_count() -> Result<()> {
        let _serial = SERIAL.lock().await;
        let step = ActionStep::Confirm;
        let error_before = count(step, ActionOutcome::Error);
        let ok_before = count(step, ActionOutcome::Ok);

        // A threshold of 0 fails the inline validation, which runs first.
        let status = post(
            "/governance/confirm",
            serde_json::json!({
                "party_id": party_id()?.to_string(),
                "rules_contract_id": "00rules",
                "action": { "type": "governance_set_threshold", "new_threshold": 0 },
            }),
        )
        .await?;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(count(step, ActionOutcome::Error), error_before);
        assert_eq!(count(step, ActionOutcome::Ok), ok_before);
        Ok(())
    }
}

#[cfg(test)]
mod mediator_timeout_tests {
    use std::{
        convert::Infallible,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use actix_web::{App, http::header::AUTHORIZATION, test as actix_test};
    use canton_proto_rs::com::daml::ledger::api::v2::{
        Event, SubmitAndWaitForTransactionResponse, SubmitAndWaitResponse, Transaction, event,
    };
    use sqlx::SqlitePool;
    use tokio::sync::Notify;
    use tonic::{
        body::Body,
        codegen::{BoxFuture, Context as TaskContext, Poll, Service, http},
        server::NamedService,
        transport::{Server, server::TcpIncoming},
    };

    use super::*;
    use crate::{
        auth::MockAuthRegistry,
        db::MIGRATOR,
        server::{ledger_rejection::fixtures, middleware::AuthMiddleware},
        signing::fake_admin_api::unary,
    };

    const OFFLINE_MEMBER: &str = "member-c::1220cccc";

    /// The Ledger API command service of a participant whose submissions the
    /// mediator rejects. Each `SubmitAndWait` signals `submitted`, then
    /// answers with `rejection` once `release` is notified. A
    /// `SubmitAndWaitForTransaction` is rejected the same way, unless
    /// `created_proposal` is set: then it answers with a transaction that
    /// creates that contract. Without a `rejection`, every `SubmitAndWait`
    /// succeeds at once. `deadlines` keeps the `grpc-timeout` header of every
    /// submission. The router answers every other service as
    /// unimplemented, so the package-ref lookup falls back to the configured
    /// package.
    #[derive(Clone)]
    pub(super) struct RejectingLedger {
        pub(super) rejection: Option<tonic::Status>,
        created_proposal: Option<String>,
        submitted: Arc<Notify>,
        pub(super) release: Arc<Notify>,
        deadlines: Arc<Mutex<Vec<Option<String>>>>,
        /// How long the answer that creates `created_proposal` takes.
        create_delay: Duration,
        /// Whether that answer leaves out the created contract, as a
        /// transaction without events does.
        hide_created_contract: bool,
    }

    impl RejectingLedger {
        pub(super) fn new() -> Self {
            Self {
                rejection: Some(fixtures::mediator_timeout(OFFLINE_MEMBER)),
                created_proposal: None,
                submitted: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
                deadlines: Arc::default(),
                create_delay: Duration::ZERO,
                hide_created_contract: false,
            }
        }

        /// A ledger whose participants all answer, so every submission
        /// succeeds.
        fn accepting() -> Self {
            Self {
                rejection: None,
                ..Self::new()
            }
        }

        /// The milliseconds each submission's `grpc-timeout` allowed.
        fn deadline_millis(&self) -> Result<Vec<u64>> {
            let deadlines = self.deadlines.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            deadlines
                .iter()
                .map(|header| {
                    let header = header.as_deref().context("a submission had no deadline")?;
                    let (value, unit) = header.split_at(header.len() - 1);
                    let value: u64 = value.parse()?;
                    Ok(match unit {
                        "S" => value * 1_000,
                        "m" => value,
                        "u" => value / 1_000,
                        "n" => value / 1_000_000,
                        other => anyhow::bail!("unexpected grpc-timeout unit {other}"),
                    })
                })
                .collect()
        }

        /// Serves the ledger on a free local port and returns the port.
        fn serve(&self) -> Result<u16> {
            let incoming = TcpIncoming::bind("127.0.0.1:0".parse()?)?;
            let port = incoming.local_addr()?.port();
            tokio::spawn(
                Server::builder()
                    .add_service(self.clone())
                    .serve_with_incoming(incoming),
            );
            Ok(port)
        }
    }

    impl Service<http::Request<Body>> for RejectingLedger {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = BoxFuture<Self::Response, Infallible>;

        fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<Body>) -> Self::Future {
            let ledger = self.clone();
            Box::pin(async move {
                let path = request.uri().path().to_string();
                let method = path.strip_prefix(&format!("/{}/", Self::NAME));
                if let (Some("SubmitAndWait" | "SubmitAndWaitForTransaction"), Ok(mut deadlines)) =
                    (method, ledger.deadlines.lock())
                {
                    let deadline = request.headers().get("grpc-timeout");
                    deadlines.push(deadline.and_then(|v| v.to_str().ok()).map(String::from));
                }
                match (method, ledger.created_proposal) {
                    (Some("SubmitAndWaitForTransaction"), Some(contract_id)) => {
                        tokio::time::sleep(ledger.create_delay).await;
                        let mut response = created_transaction(contract_id);
                        if let (true, Some(transaction)) =
                            (ledger.hide_created_contract, response.transaction.as_mut())
                        {
                            transaction.events.clear();
                        }
                        return Ok(
                            unary(request, move |_: SubmitAndWaitForTransactionRequest| {
                                Ok(response.clone())
                            })
                            .await,
                        );
                    }
                    (Some("SubmitAndWait" | "SubmitAndWaitForTransaction"), _) => {}
                    _ => return Ok(tonic::Status::unimplemented("").into_http()),
                }
                let Some(rejection) = ledger.rejection else {
                    return Ok(unary(request, |_: SubmitAndWaitRequest| {
                        Ok(SubmitAndWaitResponse::default())
                    })
                    .await);
                };
                ledger.submitted.notify_one();
                ledger.release.notified().await;
                Ok(rejection.into_http())
            })
        }
    }

    impl NamedService for RejectingLedger {
        const NAME: &'static str = "com.daml.ledger.api.v2.CommandService";
    }

    /// The answer to a submission whose transaction creates `contract_id`.
    fn created_transaction(contract_id: String) -> SubmitAndWaitForTransactionResponse {
        SubmitAndWaitForTransactionResponse {
            transaction: Some(Transaction {
                update_id: "00update".to_string(),
                events: vec![Event {
                    event: Some(event::Event::Created(CreatedEvent {
                        contract_id,
                        ..Default::default()
                    })),
                }],
                ..Default::default()
            }),
        }
    }

    /// Mock credentials, and the Ledger API at `ledger_port`.
    async fn state(pool: SqlitePool, ledger_port: u16) -> Result<web::Data<AppState>> {
        let mut state = AppState::for_test_on(None, pool).into_inner();
        Arc::get_mut(&mut state)
            .context("a fresh test state has one owner")?
            .config
            .canton
            .ledger_api_port = ledger_port;
        let mock = MockAuthRegistry::new(state.party_credentials.clone());
        *state.auth.write().await = Some(WorkflowAuth::Mock(Arc::new(mock)));
        Ok(web::Data::from(state))
    }

    fn party() -> String {
        format!("dec::1220{}", "ab".repeat(32))
    }

    fn expire() -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri("/governance/expire")
            .insert_header((AUTHORIZATION, "Bearer any-token"))
            .set_json(serde_json::json!({
                "party_id": party(),
                "rules_contract_id": "00rules",
                "confirmation_cid": "00c0ffee",
            }))
    }

    /// The `(status, error_message)` of the audit row of `event`, once the
    /// detached write lands.
    async fn audit_row(pool: &SqlitePool, event: AuditEvent) -> Result<(String, Option<String>)> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let row = sqlx::query_as(
                    "SELECT status, error_message FROM governance_audit WHERE event_type = ?",
                )
                .bind(event.as_str())
                .fetch_optional(pool)
                .await?;
                if let Some(row) = row {
                    return Ok(row);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("no audit row was written")?
    }

    /// The latest expire attempt wins. A failure that never reached the
    /// mediator says nothing about the confirmer, so it keeps the record.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn expire_outcomes_keep_the_latest_timeout_until_the_confirmation_is_archived(
        pool: SqlitePool,
    ) -> Result {
        let party = CantonId::parse(&party())?;
        let timeout = Err(anyhow::Error::new(fixtures::mediator_timeout(
            OFFLINE_MEMBER,
        )));
        remember_expire_outcome(&pool, &party, "00conf", &timeout).await;
        remember_expire_outcome(&pool, &party, "00other", &timeout).await;

        let unrelated_failure = Err(anyhow::anyhow!("connection refused"));
        remember_expire_outcome(&pool, &party, "00conf", &unrelated_failure).await;
        let rows = pool.get_mediator_timeouts(&party).await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].contract_id, "00conf");
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&rows[0].unresponsive)?,
            vec![OFFLINE_MEMBER]
        );

        let later_timeout = Err(anyhow::Error::new(fixtures::mediator_timeout(
            "member-d::1220dddd",
        )));
        remember_expire_outcome(&pool, &party, "00conf", &later_timeout).await;
        let rows = pool.get_mediator_timeouts(&party).await?;
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&rows[0].unresponsive)?,
            vec!["member-d::1220dddd"]
        );

        remember_expire_outcome(&pool, &party, "00conf", &Ok(())).await;

        let rows = pool.get_mediator_timeouts(&party).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].contract_id, "00other");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_mediator_timeout_answers_504_and_names_the_party(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let ledger = RejectingLedger::new();
        ledger.release.notify_one();
        let app = actix_test::init_service(
            App::new()
                .app_data(state(pool.clone(), ledger.serve()?).await?)
                .wrap(AuthMiddleware)
                .service(expire_confirmation),
        )
        .await;

        let response = actix_test::call_service(&app, expire().to_request()).await;

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        let error = body["error"].as_str().context("no error in the body")?;
        assert!(
            error.starts_with("Failed to expire confirmation: the mediator timed out"),
            "{error}"
        );
        assert!(
            error.contains(&format!("Unresponsive: {OFFLINE_MEMBER}.")),
            "{error}"
        );
        let (status, message) = audit_row(&pool, AuditEvent::Expire).await?;
        assert_eq!(status, "failed");
        assert_eq!(message.as_deref(), Some(error));
        Ok(())
    }

    /// A proxy that hangs up makes the server drop the handler. Every write
    /// still ends, and its audit row names the unresponsive party.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_dropped_request_still_writes_the_audit_row(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        for (event, uri, _, body) in writes() {
            let ledger = RejectingLedger::new();
            let app = actix_test::init_service(
                App::new()
                    .app_data(state(pool.clone(), ledger.serve()?).await?)
                    .wrap(AuthMiddleware)
                    .service(propose_action)
                    .service(confirm_action)
                    .service(execute_action)
                    .service(expire_confirmation)
                    .service(cancel_confirmation)
                    .service(cancel_proposal),
            )
            .await;
            let request = actix_test::TestRequest::post()
                .uri(uri)
                .insert_header((AUTHORIZATION, "Bearer any-token"))
                .set_json(body);

            // Dropping the request future is what the server does to a
            // handler whose client hung up.
            tokio::select! {
                _ = actix_test::call_service(&app, request.to_request()) => {
                    anyhow::bail!("{event:?} answered before the ledger did");
                }
                () = ledger.submitted.notified() => {}
            }
            ledger.release.notify_one();

            let (status, message) = audit_row(&pool, event).await?;
            assert_eq!(status, "failed", "{event:?}");
            let message = message.unwrap_or_default();
            assert!(
                message.contains(&format!("Unresponsive: {OFFLINE_MEMBER}.")),
                "{event:?}: {message}"
            );
            let party = CantonId::parse(&party())?;
            let rows = pool.get_mediator_timeouts(&party).await?;
            // Only an expire keeps its timeout.
            if !matches!(event, AuditEvent::Expire) {
                assert!(rows.is_empty(), "{event:?}");
                continue;
            }
            assert_eq!(rows.len(), 1, "{event:?}");
            assert_eq!(rows[0].contract_id, "00c0ffee", "{event:?}");
            assert_eq!(
                serde_json::from_str::<Vec<String>>(&rows[0].unresponsive)?,
                vec![OFFLINE_MEMBER],
                "{event:?}"
            );
            pool.clear_mediator_timeout(&party, "00c0ffee").await?;
        }
        Ok(())
    }

    /// Posts `body` to `uri` as an admin through all six governance write
    /// handlers, against `ledger`. Returns the status and the `error` field.
    pub(super) async fn post_to_writes(
        pool: &SqlitePool,
        ledger: &RejectingLedger,
        uri: &str,
        body: serde_json::Value,
    ) -> Result<(StatusCode, String)> {
        let app = actix_test::init_service(
            App::new()
                .app_data(state(pool.clone(), ledger.serve()?).await?)
                .wrap(AuthMiddleware)
                .service(propose_action)
                .service(confirm_action)
                .service(execute_action)
                .service(expire_confirmation)
                .service(cancel_confirmation)
                .service(cancel_proposal),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri(uri)
            .insert_header((AUTHORIZATION, "Bearer any-token"))
            .set_json(body)
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        let status = response.status();
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        let error = body["error"].as_str().context("no error in the body")?;
        Ok((status, error.to_string()))
    }

    /// Every governance write: its audit event, its path, the context its
    /// failure message starts with, and a body the handler accepts.
    fn writes() -> [(AuditEvent, &'static str, &'static str, serde_json::Value); 6] {
        let party = party();
        let action = serde_json::json!({ "type": "governance_set_threshold", "new_threshold": 2 });
        [
            (
                AuditEvent::Propose,
                "/governance/propose",
                "Failed to create proposal",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "proposal": { "type": "generic_vote", "description": "timeout" },
                }),
            ),
            (
                AuditEvent::Confirm,
                "/governance/confirm",
                "Failed to submit confirmation",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": action,
                }),
            ),
            (
                AuditEvent::Execute,
                "/governance/execute",
                "Failed to execute action",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": action,
                    "confirmation_cids": ["00c0ffee"],
                }),
            ),
            (
                AuditEvent::Expire,
                "/governance/expire",
                "Failed to expire confirmation",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "confirmation_cid": "00c0ffee",
                }),
            ),
            (
                AuditEvent::Cancel,
                "/governance/cancel",
                "Failed to cancel confirmation",
                serde_json::json!({ "party_id": party, "confirmation_cid": "00c0ffee" }),
            ),
            (
                AuditEvent::CancelProposal,
                "/governance/cancel-proposal",
                "Failed to cancel proposal",
                serde_json::json!({ "party_id": party, "proposal_cid": "00p" }),
            ),
        ]
    }

    /// Issue #615 asks that every governance submission the mediator times
    /// out names the unresponsive party, not only expire and cancel.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn every_governance_write_names_the_unresponsive_party(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        for (event, uri, context, body) in writes() {
            let ledger = RejectingLedger::new();
            ledger.release.notify_one();

            let (status, error) = post_to_writes(&pool, &ledger, uri, body).await?;

            assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{event:?}: {error}");
            assert_eq!(
                error,
                format!(
                    "{context}: the mediator timed out waiting for confirmations \
                     (MEDIATOR_SAYS_TX_TIMED_OUT). Unresponsive: {OFFLINE_MEMBER}. The \
                     participant that hosts each of these parties must be online to confirm."
                ),
                "{event:?}"
            );
            let (audit_status, audit_message) = audit_row(&pool, event).await?;
            assert_eq!(audit_status, "failed", "{event:?}");
            assert_eq!(audit_message, Some(error), "{event:?}");
        }
        Ok(())
    }

    /// A submission without a deadline keeps its task alive until Canton
    /// stops tracking the command, long after the caller hung up.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn every_governance_submission_carries_a_deadline(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        for (event, uri, _, body) in writes() {
            let mut ledger = RejectingLedger::new();
            // The proposer's confirmation is a second submission; reach it.
            ledger.created_proposal = Some("00proposal".to_string());
            ledger.release.notify_one();

            post_to_writes(&pool, &ledger, uri, body).await?;

            let deadlines = ledger.deadline_millis()?;
            let expected = if matches!(event, AuditEvent::Propose) {
                2
            } else {
                1
            };
            assert_eq!(deadlines.len(), expected, "{event:?}");
            for millis in deadlines {
                assert!(
                    (129_000..=130_000).contains(&millis),
                    "{event:?}: {millis} ms"
                );
            }
        }
        Ok(())
    }

    /// A propose makes two submissions. They share one budget, so the whole
    /// write waits no longer than any other.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_propose_shares_one_deadline_across_its_submissions(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let mut ledger = RejectingLedger::new();
        ledger.created_proposal = Some("00proposal".to_string());
        ledger.create_delay = Duration::from_millis(1_500);
        ledger.release.notify_one();
        let (_, uri, _, body) = writes()
            .into_iter()
            .find(|(event, ..)| matches!(event, AuditEvent::Propose))
            .context("no propose case")?;

        post_to_writes(&pool, &ledger, uri, body).await?;

        let [create, confirm] = ledger.deadline_millis()?[..] else {
            anyhow::bail!("expected two submissions");
        };
        assert!(create <= 130_000, "{create} ms");
        assert!(
            confirm <= 128_500,
            "the confirmation got a fresh budget: {confirm} ms"
        );
        Ok(())
    }

    /// A submission that stopped waiting before the ledger answered may still
    /// land. A propose retried then would create a second proposal, so the
    /// answer must say that the outcome is unknown.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_submission_that_ran_out_of_time_has_an_unknown_outcome(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let rejections = [
            (
                tonic::Status::deadline_exceeded("deadline passed"),
                "DEADLINE_EXCEEDED",
            ),
            // The gRPC client's answer when DecMan's own deadline passes.
            (tonic::Status::cancelled("Timeout expired"), "CANCELLED"),
            (fixtures::request_timeout(), "REQUEST_TIME_OUT"),
        ];
        for (rejection, cause) in rejections {
            for (event, uri, context, body) in writes() {
                let mut ledger = RejectingLedger::new();
                ledger.rejection = Some(rejection.clone());
                ledger.release.notify_one();

                let (status, error) = post_to_writes(&pool, &ledger, uri, body).await?;

                assert_eq!(
                    status,
                    StatusCode::GATEWAY_TIMEOUT,
                    "{event:?} {cause}: {error}"
                );
                assert_eq!(
                    error,
                    format!(
                        "{context}: the outcome is unknown. DecMan stopped waiting before the \
                         ledger answered ({cause}), and the write may still complete. Run \
                         `governance pending` before you retry."
                    ),
                    "{event:?}"
                );
                let (audit_status, audit_message) = audit_row(&pool, event).await?;
                assert_eq!(audit_status, "failed", "{event:?}");
                assert_eq!(audit_message, Some(error), "{event:?}");
                sqlx::query("DELETE FROM governance_audit")
                    .execute(&pool)
                    .await?;
            }
        }
        Ok(())
    }

    async fn a_panicking_submission() -> Submitted {
        panic!("the submission failed in a way no handler maps")
    }

    /// A submission that ends without an outcome still leaves a failed row,
    /// so the audit trail never misses a write the node attempted.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_submission_that_panics_still_writes_a_failed_audit_row(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let record = WriteRecord {
            step: ActionStep::Execute,
            party_id: CantonId::parse(&party())?,
            member_party_id: CantonId::parse(&party())?,
            governance_type: GovernanceType::CoreSelf,
            action_summary: "governance_set_threshold".to_string(),
            details: "{}".to_string(),
        };

        let response = run_write(pool.clone(), record, "unused", a_panicking_submission()).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let (status, message) = audit_row(&pool, AuditEvent::Execute).await?;
        assert_eq!(status, "failed");
        let message = message.unwrap_or_default();
        assert!(
            message.starts_with("The execute submission ended without an outcome"),
            "{message}"
        );
        // The panic text is internal and goes to the server log only.
        assert!(!message.contains("no handler maps"), "{message}");
        Ok(())
    }

    /// A client may read the audit trail as soon as it has the answer, so the
    /// row must be written by then.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_audit_row_is_written_before_the_answer(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        for (event, uri, _, body) in writes() {
            let ledger = RejectingLedger::new();
            ledger.release.notify_one();

            let (status, error) = post_to_writes(&pool, &ledger, uri, body).await?;

            let row: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT status, error_message FROM governance_audit WHERE event_type = ?",
            )
            .bind(event.as_str())
            .fetch_optional(&pool)
            .await?;
            assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{event:?}");
            assert_eq!(row, Some(("failed".to_string(), Some(error))), "{event:?}");
        }
        Ok(())
    }

    /// The Ledger API error names its address. Any signed-in user can read
    /// the audit trail, so the answer and the row keep only the category.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_ledger_connection_hides_its_address(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        for (event, uri, context, body) in writes() {
            let app = actix_test::init_service(
                App::new()
                    .app_data(state(pool.clone(), 1).await?)
                    .wrap(AuthMiddleware)
                    .service(propose_action)
                    .service(confirm_action)
                    .service(execute_action)
                    .service(expire_confirmation)
                    .service(cancel_confirmation)
                    .service(cancel_proposal),
            )
            .await;
            let request = actix_test::TestRequest::post()
                .uri(uri)
                .insert_header((AUTHORIZATION, "Bearer any-token"))
                .set_json(body)
                .to_request();

            let response = actix_test::call_service(&app, request).await;

            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{event:?}"
            );
            let body: serde_json::Value = actix_test::read_body_json(response).await;
            let error = body["error"].as_str().context("no error in the body")?;
            let context = if matches!(event, AuditEvent::Propose) {
                "Failed to create proposal"
            } else {
                context
            };
            assert_eq!(
                error,
                format!("{context}: the Ledger API connection failed. See the DecMan server log."),
                "{event:?}"
            );
            let (_, message) = audit_row(&pool, event).await?;
            assert_eq!(message.as_deref(), Some(error), "{event:?}");
        }
        Ok(())
    }

    /// The server log keeps the whole error chain. Its line names the party
    /// and the write, so an operator can find the failure a caller reports.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_logged_failure_names_the_party_and_the_write(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        for (event, uri, _, body) in writes() {
            let write = match event {
                AuditEvent::Propose => "propose",
                AuditEvent::Confirm => "confirm",
                AuditEvent::Execute => "execute",
                AuditEvent::Expire => "expire",
                AuditEvent::Cancel => "cancel_confirmation",
                AuditEvent::CancelProposal => "cancel_proposal",
                other => anyhow::bail!("{other:?} is not a write"),
            };
            let app = actix_test::init_service(
                App::new()
                    .app_data(state(pool.clone(), 1).await?)
                    .wrap(AuthMiddleware)
                    .service(propose_action)
                    .service(confirm_action)
                    .service(execute_action)
                    .service(expire_confirmation)
                    .service(cancel_confirmation)
                    .service(cancel_proposal),
            )
            .await;
            let request = actix_test::TestRequest::post()
                .uri(uri)
                .insert_header((AUTHORIZATION, "Bearer any-token"))
                .set_json(body)
                .to_request();

            let buffer = super::get_party_credentials_tests::LogBuffer::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(buffer.clone())
                .with_ansi(false)
                .finish();
            let response = {
                let _guard = tracing::subscriber::set_default(subscriber);
                actix_test::call_service(&app, request).await
            };

            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{event:?}"
            );
            let logs = buffer.captured()?;
            let line = logs
                .lines()
                .find(|line| line.contains("127.0.0.1:1"))
                .with_context(|| format!("{event:?} logged no error chain: {logs}"))?;
            assert!(line.contains(" WARN "), "{event:?}: {line}");
            assert!(
                line.contains(&format!("party_id={}", party())),
                "{event:?}: {line}"
            );
            assert!(
                line.contains(&format!("write={write}")),
                "{event:?}: {line}"
            );
        }
        Ok(())
    }

    /// The Ledger API event service of a participant that never answers.
    #[derive(Clone)]
    struct SilentEvents;

    impl Service<http::Request<Body>> for SilentEvents {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = BoxFuture<Self::Response, Infallible>;

        fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: http::Request<Body>) -> Self::Future {
            Box::pin(std::future::pending())
        }
    }

    impl NamedService for SilentEvents {
        const NAME: &'static str = "com.daml.ledger.api.v2.EventQueryService";
    }

    /// A propose reads the rules contract's package between its two
    /// submissions. That read shares the write's deadline, so a ledger that
    /// never answers it cannot stretch the write.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_propose_stops_at_its_deadline_during_the_rules_read(pool: SqlitePool) -> Result {
        let mut ledger = RejectingLedger::new();
        ledger.created_proposal = Some("00proposal".to_string());
        let incoming = TcpIncoming::bind("127.0.0.1:0".parse()?)?;
        let port = incoming.local_addr()?.port();
        tokio::spawn(
            Server::builder()
                .add_service(ledger)
                .add_service(SilentEvents)
                .serve_with_incoming(incoming),
        );
        let data = state(pool, port).await?;
        let body: ProposeActionRequest = serde_json::from_value(serde_json::json!({
            "party_id": party(),
            "rules_contract_id": "00rules",
            "proposal": { "type": "generic_vote", "description": "deadline" },
        }))?;
        let member = CantonId::parse(&format!("member::1220{}", "cd".repeat(32)))?;

        let proposing = submit_proposal(
            data,
            Commands::default(),
            "any-token".to_string(),
            member,
            body,
            Instant::now() + Duration::from_millis(500),
        );
        let submitted = tokio::time::timeout(Duration::from_secs(5), proposing)
            .await
            .context("the propose outlived its deadline")?;

        let (status, error) = submitted.failure.context("the propose succeeded")?;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{error}");
        assert!(
            error.ends_with("Confirm proposal 00proposal; do not re-propose."),
            "{error}"
        );
        assert_eq!(submitted.proposal_cid.as_deref(), Some("00proposal"));
        Ok(())
    }

    /// The proposer's own confirmation can time out after the proposal is on
    /// the ledger. The answer must give the new proposal's id, so the operator
    /// confirms it instead of proposing a second time.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_timed_out_proposer_confirmation_names_the_created_proposal(
        pool: SqlitePool,
    ) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let mut ledger = RejectingLedger::new();
        ledger.created_proposal = Some("00proposal".to_string());
        ledger.release.notify_one();

        let (status, error) = post_to_writes(
            &pool,
            &ledger,
            "/governance/propose",
            serde_json::json!({
                "party_id": party(),
                "rules_contract_id": "00rules",
                "proposal": { "type": "generic_vote", "description": "timeout" },
            }),
        )
        .await?;

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{error}");
        assert!(
            error.starts_with(
                "Proposal created (CID: 00proposal) but confirmation failed: the mediator \
                 timed out"
            ),
            "{error}"
        );
        assert!(
            error.contains(&format!("Unresponsive: {OFFLINE_MEMBER}.")),
            "{error}"
        );
        assert!(
            error.ends_with("Confirm proposal 00proposal; do not re-propose."),
            "{error}"
        );
        let (audit_status, audit_message) = audit_row(&pool, AuditEvent::Propose).await?;
        assert_eq!(audit_status, "failed");
        assert_eq!(audit_message, Some(error));
        Ok(())
    }

    /// The proposal exists even when the answer does not name it, so the
    /// operator must find it rather than propose again.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_created_proposal_without_its_id_says_not_to_re_propose(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let mut ledger = RejectingLedger::new();
        ledger.created_proposal = Some("00proposal".to_string());
        ledger.hide_created_contract = true;

        let (status, error) = post_to_writes(
            &pool,
            &ledger,
            "/governance/propose",
            serde_json::json!({
                "party_id": party(),
                "rules_contract_id": "00rules",
                "proposal": { "type": "generic_vote", "description": "no id" },
            }),
        )
        .await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{error}");
        assert_eq!(
            error,
            "Proposal created in update 00update but could not extract its contract ID. Find \
             it in `governance pending` and confirm it there; do not re-propose."
        );
        let (audit_status, audit_message) = audit_row(&pool, AuditEvent::Propose).await?;
        assert_eq!(audit_status, "failed");
        assert_eq!(audit_message, Some(error));
        Ok(())
    }

    /// The other tests have the ledger answer with a status. Here the ledger
    /// never answers, so only DecMan's own deadline ends the submission. The
    /// gRPC client must stop at that deadline, and its error must read as an
    /// unknown outcome, not as a failed write.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn decmans_own_deadline_ends_a_silent_submission_as_unknown(pool: SqlitePool) -> Result {
        let ledger = RejectingLedger::new();
        let data = state(pool, ledger.serve()?).await?;
        let mut client = CommandServiceClient::new(data.config.ledger_channel().await?);
        let request = ledger_request(
            SubmitAndWaitRequest::default(),
            "any-token",
            Instant::now() + Duration::from_millis(300),
        )?;

        let started = Instant::now();
        let submitted =
            tokio::time::timeout(Duration::from_secs(10), client.submit_and_wait(request))
                .await
                .context("the submission outlived its deadline")?;

        assert!(started.elapsed() < Duration::from_secs(5));
        let error = anyhow::Error::new(
            submitted
                .err()
                .context("a ledger that never answers cannot accept")?,
        );
        let (status, message) = submission_failure("Failed to expire confirmation", &error);
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{message}");
        assert!(message.contains("the outcome is unknown"), "{message}");
        Ok(())
    }

    /// The record follows the latest expire attempt through the handler: a
    /// timeout that names another party replaces it, and a success, for
    /// example after the confirmer's participant came back, forgets it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_latest_expire_attempt_decides_the_recorded_timeout(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let party = CantonId::parse(&party())?;
        let body = serde_json::json!({
            "party_id": party,
            "rules_contract_id": "00rules",
            "confirmation_cid": "00c0ffee",
        });
        let unresponsive = |rows: &[GovernanceMediatorTimeoutRow]| -> Result<Vec<String>> {
            let [row] = rows else {
                anyhow::bail!("expected one record, found {}", rows.len());
            };
            assert_eq!(row.contract_id, "00c0ffee");
            Ok(serde_json::from_str(&row.unresponsive)?)
        };

        let offline = RejectingLedger::new();
        offline.release.notify_one();
        let (status, _) =
            post_to_writes(&pool, &offline, "/governance/expire", body.clone()).await?;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        let rows = pool.get_mediator_timeouts(&party).await?;
        assert_eq!(unresponsive(&rows)?, vec![OFFLINE_MEMBER]);

        let mut another = RejectingLedger::new();
        another.rejection = Some(fixtures::mediator_timeout("member-d::1220dddd"));
        another.release.notify_one();
        let (status, _) =
            post_to_writes(&pool, &another, "/governance/expire", body.clone()).await?;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        let rows = pool.get_mediator_timeouts(&party).await?;
        assert_eq!(unresponsive(&rows)?, vec!["member-d::1220dddd"]);

        let app = actix_test::init_service(
            App::new()
                .app_data(state(pool.clone(), RejectingLedger::accepting().serve()?).await?)
                .wrap(AuthMiddleware)
                .service(expire_confirmation),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri("/governance/expire")
            .insert_header((AUTHORIZATION, "Bearer any-token"))
            .set_json(body)
            .to_request();
        let response = actix_test::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(pool.get_mediator_timeouts(&party).await?.is_empty());
        Ok(())
    }

    /// `_Cancel` is controlled by the confirmer, this node's own member. A
    /// cancel timeout therefore never shows that the confirmer is gone.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cancel_timeout_is_not_recorded(pool: SqlitePool) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let party = CantonId::parse(&party())?;
        let ledger = RejectingLedger::new();
        ledger.release.notify_one();

        let (status, _) = post_to_writes(
            &pool,
            &ledger,
            "/governance/cancel",
            serde_json::json!({ "party_id": party, "confirmation_cid": "00c0ffee" }),
        )
        .await?;

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert!(pool.get_mediator_timeouts(&party).await?.is_empty());
        Ok(())
    }
}

/// `GET /governance/confirmations` read against a participant's ACS.
#[cfg(test)]
mod confirmations_feed_tests {
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use actix_web::{App, test as actix_test};
    use canton_proto_rs::com::daml::ledger::api::v2::{
        ActiveContract, GenMap, GetActiveContractsPageRequest, GetActiveContractsPageResponse,
        GetActiveContractsResponse, GetLedgerEndRequest, GetLedgerEndResponse, Identifier, Record,
        RecordField, Value, cumulative_filter::IdentifierFilter, gen_map,
        get_active_contracts_response::ContractEntry, value,
    };
    use serde_json::json;
    use sqlx::SqlitePool;
    use tonic::{
        body::Body,
        codegen::{BoxFuture, Context as TaskContext, Poll, Service, http},
        server::NamedService,
        transport::{Server, server::TcpIncoming},
    };

    use super::*;
    use crate::{
        auth::MockAuthRegistry, db::MIGRATOR, server::ledger_rejection::fixtures,
        signing::fake_admin_api::unary,
    };

    /// The Ledger API state service of a participant whose ACS holds
    /// `contracts`. A page read returns, in one page, every contract whose
    /// template matches a template filter of the request. An interface read,
    /// such as the one for proposals, returns `proposals`, a page of the
    /// requested size at a time, and `proposal_reads` counts those reads. A
    /// read of the `missing` template fails the way Canton fails it when the
    /// package is uploaded but lacks the template. A read of the `refused`
    /// template fails with a permission error.
    #[derive(Clone, Default)]
    struct AcsLedger {
        contracts: Arc<Vec<CreatedEvent>>,
        missing: Option<&'static str>,
        proposals: Arc<Vec<CreatedEvent>>,
        proposal_reads: Arc<AtomicUsize>,
        refused: Option<&'static str>,
    }

    impl AcsLedger {
        /// Serves the ledger on a free local port and returns the port.
        fn serve(&self) -> Result<u16> {
            let incoming = TcpIncoming::bind("127.0.0.1:0".parse()?)?;
            let port = incoming.local_addr()?.port();
            tokio::spawn(
                Server::builder()
                    .add_service(self.clone())
                    .serve_with_incoming(incoming),
            );
            Ok(port)
        }

        fn page(
            &self,
            request: &GetActiveContractsPageRequest,
        ) -> std::result::Result<GetActiveContractsPageResponse, tonic::Status> {
            let wanted: Vec<(&str, &str)> = request
                .event_format
                .iter()
                .flat_map(|format| format.filters_by_party.values())
                .flat_map(|filters| &filters.cumulative)
                .filter_map(|filter| match &filter.identifier_filter {
                    Some(IdentifierFilter::TemplateFilter(t)) => t.template_id.as_ref(),
                    _ => None,
                })
                .map(|id| (id.module_name.as_str(), id.entity_name.as_str()))
                .collect();
            if let Some(missing) = self.missing.filter(|m| wanted.iter().any(|w| w.1 == *m)) {
                return Err(tonic::Status::invalid_argument(format!(
                    "NO_TEMPLATES_FOR_PACKAGE_NAME_AND_QUALIFIED_NAME(8,0): no template {missing}"
                )));
            }
            if let Some(refused) = self.refused.filter(|r| wanted.iter().any(|w| w.1 == *r)) {
                return Err(tonic::Status::permission_denied(format!(
                    "PERMISSION_DENIED(7,0): cannot read {refused} at ledger.internal:5001"
                )));
            }
            let reads_proposals = request
                .event_format
                .iter()
                .flat_map(|format| format.filters_by_party.values())
                .flat_map(|filters| &filters.cumulative)
                .any(|filter| {
                    matches!(
                        filter.identifier_filter,
                        Some(IdentifierFilter::InterfaceFilter(_))
                    )
                });
            if reads_proposals {
                self.proposal_reads.fetch_add(1, Ordering::SeqCst);
                return Ok(self.proposal_page(request));
            }
            let active_contracts = self
                .contracts
                .iter()
                .filter(|c| {
                    c.template_id.as_ref().is_some_and(|t| {
                        wanted.contains(&(t.module_name.as_str(), t.entity_name.as_str()))
                    })
                })
                .map(|c| GetActiveContractsResponse {
                    contract_entry: Some(ContractEntry::ActiveContract(ActiveContract {
                        created_event: Some(c.clone()),
                        ..Default::default()
                    })),
                    ..Default::default()
                })
                .collect();
            Ok(GetActiveContractsPageResponse {
                active_contracts,
                active_at_offset: 1,
                next_page_token: None,
            })
        }

        /// The page of `proposals` that `request` asks for. The page token is
        /// the index of its first proposal.
        fn proposal_page(
            &self,
            request: &GetActiveContractsPageRequest,
        ) -> GetActiveContractsPageResponse {
            let start = request
                .page_token
                .as_deref()
                .and_then(|token| std::str::from_utf8(token).ok()?.parse().ok())
                .unwrap_or(0usize);
            let size = request.max_page_size.unwrap_or(1).max(1) as usize;
            let end = (start + size).min(self.proposals.len());
            GetActiveContractsPageResponse {
                active_contracts: self.proposals[start..end]
                    .iter()
                    .map(|c| GetActiveContractsResponse {
                        contract_entry: Some(ContractEntry::ActiveContract(ActiveContract {
                            created_event: Some(c.clone()),
                            ..Default::default()
                        })),
                        ..Default::default()
                    })
                    .collect(),
                active_at_offset: 1,
                next_page_token: (end < self.proposals.len()).then(|| end.to_string().into_bytes()),
            }
        }
    }

    impl Service<http::Request<Body>> for AcsLedger {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = BoxFuture<Self::Response, Infallible>;

        fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<Body>) -> Self::Future {
            let ledger = self.clone();
            Box::pin(async move {
                let path = request.uri().path().to_string();
                Ok(match path.strip_prefix(&format!("/{}/", Self::NAME)) {
                    Some("GetLedgerEnd") => {
                        unary(request, |_: GetLedgerEndRequest| {
                            Ok(GetLedgerEndResponse {
                                offset: 1,
                                ..Default::default()
                            })
                        })
                        .await
                    }
                    Some("GetActiveContractsPage") => {
                        unary(request, move |r: GetActiveContractsPageRequest| {
                            ledger.page(&r)
                        })
                        .await
                    }
                    _ => tonic::Status::unimplemented("").into_http(),
                })
            })
        }
    }

    impl NamedService for AcsLedger {
        const NAME: &'static str = "com.daml.ledger.api.v2.StateService";
    }

    fn id(name: &str, byte: &str) -> CantonId {
        CantonId::parse(&format!("{name}::1220{}", byte.repeat(32))).expect("valid party")
    }

    fn field(label: &str, sum: value::Sum) -> RecordField {
        RecordField {
            label: label.to_string(),
            value: Some(Value { sum: Some(sum) }),
        }
    }

    fn contract(
        contract_id: &str,
        module: &str,
        entity: &str,
        fields: Vec<RecordField>,
    ) -> CreatedEvent {
        CreatedEvent {
            contract_id: contract_id.to_string(),
            template_id: Some(Identifier {
                package_id: "00pkg".to_string(),
                module_name: module.to_string(),
                entity_name: entity.to_string(),
            }),
            create_arguments: Some(Record {
                record_id: None,
                fields,
            }),
            ..Default::default()
        }
    }

    /// The `GovernanceRules` contract of `party`, with `members`.
    fn rules(party: &CantonId, members: &[&CantonId]) -> CreatedEvent {
        let members = GenMap {
            entries: members
                .iter()
                .map(|member| gen_map::Entry {
                    key: Some(Value {
                        sum: Some(value::Sum::Party(member.to_string())),
                    }),
                    value: Some(Value {
                        sum: Some(value::Sum::Unit(())),
                    }),
                })
                .collect(),
        };
        contract(
            "00rules",
            "Governance.Rules",
            "GovernanceRules",
            vec![
                field("governanceParty", value::Sum::Party(party.to_string())),
                field("members", value::Sum::GenMap(members)),
                field("threshold", value::Sum::Int64(2)),
            ],
        )
    }

    /// A self-action confirmation by `confirmer` that expires at `expires_at`
    /// (Unix seconds).
    fn self_confirmation(
        contract_id: &str,
        action: &ActionType,
        confirmer: &CantonId,
        expires_at: i64,
    ) -> Result<CreatedEvent> {
        let action = action
            .to_self_proto()?
            .sum
            .context("an action has a value")?;
        Ok(contract(
            contract_id,
            "Governance.Rules",
            "GovernanceSelfConfirmation",
            vec![
                field("action", action),
                field("confirmer", value::Sum::Party(confirmer.to_string())),
                field("expiresAt", value::Sum::Timestamp(expires_at * 1_000_000)),
            ],
        ))
    }

    /// A confirmation by `confirmer` of the proposal `proposal_cid`.
    fn domain_confirmation(
        contract_id: &str,
        proposal_cid: &str,
        confirmer: &CantonId,
        expires_at: i64,
    ) -> CreatedEvent {
        contract(
            contract_id,
            "Governance.Confirmation",
            "GovernanceConfirmation",
            vec![
                field(
                    "actionProposalCid",
                    value::Sum::ContractId(proposal_cid.to_string()),
                ),
                field("actionLabel", value::Sum::Text("GenericVote".to_string())),
                field("confirmer", value::Sum::Party(confirmer.to_string())),
                field("expiresAt", value::Sum::Timestamp(expires_at * 1_000_000)),
            ],
        )
    }

    /// Mock credentials, and the Ledger API at `ledger_port`.
    async fn state(pool: SqlitePool, ledger_port: u16) -> Result<web::Data<AppState>> {
        let mut state = AppState::for_test_on(None, pool).into_inner();
        Arc::get_mut(&mut state)
            .context("a fresh test state has one owner")?
            .config
            .canton
            .ledger_api_port = ledger_port;
        let mock = MockAuthRegistry::new(state.party_credentials.clone());
        *state.auth.write().await = Some(WorkflowAuth::Mock(Arc::new(mock)));
        Ok(web::Data::from(state))
    }

    /// The feed of `party` against `ledger`.
    async fn feed(
        pool: &SqlitePool,
        ledger: &AcsLedger,
        party: &CantonId,
    ) -> Result<serde_json::Value> {
        feed_with(pool, ledger, party, "").await
    }

    /// The feed of `party` against `ledger`, with `extra` appended to the
    /// query string.
    async fn feed_with(
        pool: &SqlitePool,
        ledger: &AcsLedger,
        party: &CantonId,
        extra: &str,
    ) -> Result<serde_json::Value> {
        let app = actix_test::init_service(
            App::new()
                .app_data(state(pool.clone(), ledger.serve()?).await?)
                .service(get_governance),
        )
        .await;
        let request = actix_test::TestRequest::get()
            .uri(&format!(
                "/governance/confirmations?party_id={party}{extra}"
            ))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        anyhow::ensure!(
            response.status() == StatusCode::OK,
            "the feed answered {}",
            response.status()
        );
        Ok(actix_test::read_body_json(response).await)
    }

    /// The entry of `list` that holds the confirmation `contract_id`.
    fn holding<'a>(
        list: &'a serde_json::Value,
        contract_id: &str,
    ) -> Result<&'a serde_json::Value> {
        list.as_array()
            .context("not a list")?
            .iter()
            .find(|action| action["confirmations"][0]["contract_id"] == contract_id)
            .with_context(|| format!("no action holds {contract_id}"))
    }

    /// Issue #615: an action whose expired confirmations all failed their
    /// latest expire with a mediator timeout naming the confirmer is marked as
    /// one that cannot be archived. A former member without such evidence is
    /// not marked: its participant may still be online. The evidence comes
    /// from this party's own timeout records.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_feed_marks_what_cannot_be_archived(pool: SqlitePool) -> Result {
        let party = id("dec", "ab");
        let other_party = id("other", "ef");
        let (member_a, member_b, former) = (
            id("member-a", "aa"),
            id("member-b", "bb"),
            id("former", "cc"),
        );
        let now = chrono::Utc::now().timestamp();
        let (expired, live) = (now - 3600, now + 3600);
        let threshold = |new_threshold| ActionType::GovernanceSetThreshold { new_threshold };
        let ledger = AcsLedger {
            contracts: Arc::new(vec![
                rules(&party, &[&member_a, &member_b]),
                self_confirmation("00self-former", &threshold(3), &former, expired)?,
                self_confirmation("00self-live", &threshold(1), &member_a, live)?,
                self_confirmation("00self-timeout", &threshold(4), &member_b, expired)?,
                self_confirmation("00self-named-other", &threshold(5), &member_a, expired)?,
                domain_confirmation("00dom-timeout", "00gone-a", &member_a, expired),
                domain_confirmation("00dom-elsewhere", "00gone-b", &member_b, expired),
            ]),
            ..AcsLedger::default()
        };
        pool.record_mediator_timeout(&party, "00self-timeout", &[member_b.to_string()])
            .await?;
        pool.record_mediator_timeout(&party, "00self-named-other", &[member_b.to_string()])
            .await?;
        pool.record_mediator_timeout(&party, "00dom-timeout", &[member_a.to_string()])
            .await?;
        // The same confirmation id timed out for another party only.
        pool.record_mediator_timeout(&other_party, "00dom-elsewhere", &[member_b.to_string()])
            .await?;

        let body = feed(&pool, &ledger, &party).await?;

        for unmarked in ["00self-former", "00self-live", "00self-named-other"] {
            let action = holding(&body["actions"], unmarked)?;
            assert_eq!(
                action["confirmations"][0]["stuck"],
                json!(null),
                "{unmarked}"
            );
            assert_eq!(action["cannot_be_archived"], json!(false), "{unmarked}");
        }
        let timed_out = holding(&body["actions"], "00self-timeout")?;
        assert_eq!(
            timed_out["confirmations"][0]["stuck"],
            json!("participant_unresponsive")
        );
        assert_eq!(timed_out["cannot_be_archived"], json!(true));
        let timed_out = holding(&body["domain_actions"], "00dom-timeout")?;
        assert_eq!(timed_out["orphaned"], json!(true));
        assert_eq!(
            timed_out["confirmations"][0]["stuck"],
            json!("participant_unresponsive")
        );
        assert_eq!(timed_out["cannot_be_archived"], json!(true));
        // An orphan that can still be archived stays out of the list.
        assert!(holding(&body["domain_actions"], "00dom-elsewhere").is_err());
        Ok(())
    }

    /// A proposal created on the ledger for `party`.
    fn proposal(contract_id: &str, party: &CantonId, proposer: &CantonId) -> CreatedEvent {
        contract(
            contract_id,
            "Governance.Proposal",
            "GenericVote",
            vec![
                field("governanceParty", value::Sum::Party(party.to_string())),
                field("proposer", value::Sum::Party(proposer.to_string())),
            ],
        )
    }

    /// #615 asks that an operator can hide an orphan that cannot be archived.
    /// With more than one page of proposals, no single page proves its
    /// proposal gone, so the first page checks every active proposal id. An
    /// orphan that can still be archived stays out, as on one page.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_marked_orphan_is_listed_when_the_proposals_span_pages(pool: SqlitePool) -> Result {
        let party = id("dec", "ab");
        let member = id("member-a", "aa");
        let expired = chrono::Utc::now().timestamp() - 3600;
        let ledger = AcsLedger {
            contracts: Arc::new(vec![
                rules(&party, &[&member]),
                domain_confirmation("00dom-marked", "00gone", &member, expired),
                domain_confirmation("00dom-unmarked", "00gone-b", &member, expired),
            ]),
            proposals: Arc::new(
                ["00p1", "00p2", "00p3"]
                    .iter()
                    .map(|cid| proposal(cid, &party, &member))
                    .collect(),
            ),
            ..AcsLedger::default()
        };
        pool.record_mediator_timeout(&party, "00dom-marked", &[member.to_string()])
            .await?;

        let first = feed_with(&pool, &ledger, &party, "&limit=2").await?;

        let cursor = first["next_cursor"]
            .as_str()
            .context("the proposals fit one page")?;
        let marked = holding(&first["domain_actions"], "00dom-marked")?;
        assert_eq!(marked["orphaned"], json!(true));
        assert_eq!(marked["cannot_be_archived"], json!(true));
        assert!(holding(&first["domain_actions"], "00dom-unmarked").is_err());

        let second =
            feed_with(&pool, &ledger, &party, &format!("&limit=2&cursor={cursor}")).await?;
        assert!(holding(&second["domain_actions"], "00dom-marked").is_err());
        assert!(holding(&second["domain_actions"], "00dom-unmarked").is_err());
        Ok(())
    }

    /// The first page scans the active proposal ids for a marked orphan, on
    /// every read while the orphan exists. The scan stops after ten pages of
    /// 1,000 ids. It then leaves the orphan out and logs one warning.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_orphan_scan_stops_after_ten_pages(pool: SqlitePool) -> Result {
        let party = id("dec", "ab");
        let member = id("member-a", "aa");
        let expired = chrono::Utc::now().timestamp() - 3600;
        pool.record_mediator_timeout(&party, "00dom-marked", &[member.to_string()])
            .await?;
        // The feed reads one proposal, then the scan reads 1,000 a page.
        for (proposals, listed) in [(10_000, true), (10_001, false)] {
            let ledger = AcsLedger {
                contracts: Arc::new(vec![
                    rules(&party, &[&member]),
                    domain_confirmation("00dom-marked", "00gone", &member, expired),
                ]),
                proposals: Arc::new(
                    (0..proposals)
                        .map(|n| proposal(&format!("00p{n}"), &party, &member))
                        .collect(),
                ),
                ..AcsLedger::default()
            };
            let buffer = super::get_party_credentials_tests::LogBuffer::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(buffer.clone())
                .with_ansi(false)
                .finish();

            let body = {
                let _guard = tracing::subscriber::set_default(subscriber);
                feed_with(&pool, &ledger, &party, "&limit=1").await?
            };

            assert_eq!(
                holding(&body["domain_actions"], "00dom-marked").is_ok(),
                listed,
                "{proposals} proposals: {body}"
            );
            assert_eq!(
                ledger.proposal_reads.load(Ordering::SeqCst),
                11,
                "{proposals} proposals"
            );
            let logs = buffer.captured()?;
            assert_eq!(
                logs.matches("Leaving 1 marked orphans out").count(),
                usize::from(!listed),
                "{proposals} proposals: {logs}"
            );
        }
        Ok(())
    }

    /// A read that leaves confirmations out must not look like a short list.
    /// The answer names the failure, and keeps the ledger's text in the log.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_confirmation_read_answers_an_error(pool: SqlitePool) -> Result {
        let party = id("dec", "ab");
        let member = id("member-a", "aa");
        let ledger = AcsLedger {
            contracts: Arc::new(vec![rules(&party, &[&member])]),
            refused: Some("GovernanceSelfConfirmation"),
            ..AcsLedger::default()
        };
        let app = actix_test::init_service(
            App::new()
                .app_data(state(pool, ledger.serve()?).await?)
                .service(get_governance),
        )
        .await;
        let request = actix_test::TestRequest::get()
            .uri(&format!("/governance/confirmations?party_id={party}"))
            .to_request();

        let response = actix_test::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], json!(FEED_READ_FAILED));
        Ok(())
    }

    /// A package can be uploaded without one of the governance templates.
    /// The confirmations of the templates it has must still be listed.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_template_missing_from_its_package_does_not_empty_the_feed(
        pool: SqlitePool,
    ) -> Result {
        let party = id("dec", "ab");
        let member = id("member-a", "aa");
        let expired = chrono::Utc::now().timestamp() - 3600;
        let ledger = AcsLedger {
            contracts: Arc::new(vec![
                rules(&party, &[&member]),
                domain_confirmation("00dom-timeout", "00gone", &member, expired),
            ]),
            missing: Some("GovernanceSelfConfirmation"),
            ..AcsLedger::default()
        };
        pool.record_mediator_timeout(&party, "00dom-timeout", &[member.to_string()])
            .await?;

        let body = feed(&pool, &ledger, &party).await?;

        let action = holding(&body["domain_actions"], "00dom-timeout")?;
        assert_eq!(action["cannot_be_archived"], json!(true));
        Ok(())
    }

    /// A node without the `GovernableAction` interface has no proposals. The
    /// read is complete, so the pending-governance poller reports no error.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_node_without_the_proposal_interface_reads_a_complete_queue(
        pool: SqlitePool,
    ) -> Result {
        let party = id("dec", "ab");
        let member = id("member-a", "aa");
        let ledger = AcsLedger {
            contracts: Arc::new(vec![rules(&party, &[&member])]),
            ..AcsLedger::default()
        };
        let data = state(pool, ledger.serve()?).await?;
        let mut packages = packages();
        packages.governance_action = None;

        let queue = get_governance_confirmations(
            &data.config,
            &party,
            1,
            None,
            None,
            &packages,
            25,
            None,
            &ExpireTimeouts::new(),
        )
        .await?;

        assert!(queue.complete);
        Ok(())
    }

    /// The operator flow of #615 from end to end: an expire times out at the
    /// mediator, which names the confirmer as Canton writes a party id. The
    /// feed then marks the confirmation, and lists the orphan it belongs to.
    /// The other tests store the record directly, so only this one shows that
    /// the stored names match the confirmers the feed reads from the ledger.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_expire_that_times_out_naming_the_confirmer_marks_its_action(
        pool: SqlitePool,
    ) -> Result {
        let _serial = super::action_metrics_tests::SERIAL.lock().await;
        let party = id("dec", "ab");
        let confirmer = format!("member-a::1220{}", "aa".repeat(32));
        let member = CantonId::parse(&confirmer)?;
        let expired = chrono::Utc::now().timestamp() - 3600;
        let ledger = AcsLedger {
            contracts: Arc::new(vec![
                rules(&party, &[&member]),
                self_confirmation(
                    "00self",
                    &ActionType::GovernanceSetThreshold { new_threshold: 3 },
                    &member,
                    expired,
                )?,
                domain_confirmation("00dom", "00gone", &member, expired),
            ]),
            ..AcsLedger::default()
        };

        let before = feed(&pool, &ledger, &party).await?;
        assert_eq!(
            holding(&before["actions"], "00self")?["cannot_be_archived"],
            json!(false)
        );
        assert!(
            holding(&before["domain_actions"], "00dom").is_err(),
            "{before}"
        );

        for confirmation_cid in ["00self", "00dom"] {
            let mut offline = super::mediator_timeout_tests::RejectingLedger::new();
            offline.rejection = Some(fixtures::mediator_timeout(&confirmer));
            offline.release.notify_one();
            let (status, error) = super::mediator_timeout_tests::post_to_writes(
                &pool,
                &offline,
                "/governance/expire",
                json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "confirmation_cid": confirmation_cid,
                }),
            )
            .await?;
            assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{error}");
        }

        let after = feed(&pool, &ledger, &party).await?;
        for (list, cid) in [("actions", "00self"), ("domain_actions", "00dom")] {
            let action = holding(&after[list], cid)?;
            assert_eq!(
                action["confirmations"][0]["stuck"],
                json!("participant_unresponsive"),
                "{cid}"
            );
            assert_eq!(action["cannot_be_archived"], json!(true), "{cid}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod cancel_confirmation_gate_tests {
    use actix_web::{
        App,
        http::StatusCode,
        test::{self, TestRequest},
    };
    use serde_json::json;

    use super::*;
    use crate::server::middleware::AuthMiddleware;

    fn cancel_request() -> TestRequest {
        TestRequest::post()
            .uri("/governance/cancel")
            .set_json(json!({
                "party_id": format!("dec-party::{}", "0".repeat(68)),
                "confirmation_cid": "00confirmation",
            }))
    }

    /// The gate fires before the handler reads the member's stored credentials.
    #[actix_web::test]
    async fn a_request_without_a_principal_is_unauthorized() -> anyhow::Result<()> {
        let data = AppState::for_test(None).await?;
        let app = test::init_service(App::new().app_data(data).service(cancel_confirmation)).await;

        let resp = test::call_service(&app, cancel_request().to_request()).await;

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    /// A valid token without the admin role must not revoke a member's live
    /// confirmation as that member.
    #[actix_web::test]
    async fn a_non_admin_cannot_cancel_a_confirmation() -> anyhow::Result<()> {
        let data = AppState::for_test_as_non_admin().await?;
        let app = test::init_service(
            App::new()
                .app_data(data)
                .wrap(AuthMiddleware)
                .service(cancel_confirmation),
        )
        .await;

        let req = cancel_request()
            .insert_header(("Authorization", "Bearer spa.user.token"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        Ok(())
    }
}

#[cfg(test)]
mod paid_accept_disclosure_tests {
    use decman_lib::catalog::types::AppTransferContextInput;
    use decman_lib::framework::DamlProtoEncode;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;

    const ROUNDS: &str = include_str!("../testdata/scan/open_and_issuing_mining_rounds.json");
    const FEATURED_APP_RIGHT: &str = include_str!("../testdata/scan/featured_app_right.json");

    /// Round 66344 of the scan fixture.
    fn round_66344() -> scan::ScanContract {
        scan::parse_open_mining_rounds(ROUNDS)
            .expect("the fixture parses")
            .into_iter()
            .find(|r| r.round == 66344)
            .expect("the fixture lists round 66344")
            .contract
    }

    /// The create event of an `AcceptPaidCredential` proposal, encoded the
    /// way decman proposes it.
    fn proposal(open_mining_round: &str, featured_app_right: Option<&str>) -> CreatedEvent {
        let payload = AcceptPaidCredential {
            user_service_cid: "00usc".to_string(),
            credential_offer_cid: "00offer".to_string(),
            deposit_amulet_cids: vec!["00amulet".to_string()],
            app_transfer_context: AppTransferContextInput {
                amulet_rules_cid: "00rules".to_string(),
                open_mining_round_cid: open_mining_round.to_string(),
                featured_app_right_cid: featured_app_right.map(str::to_string),
            },
            deposit_amount_usd: None,
        }
        .to_daml_proto()
        .expect("the proposal encodes");
        let Some(value::Sum::Record(record)) = payload.sum else {
            panic!("a proposal encodes as a record");
        };
        CreatedEvent {
            create_arguments: Some(record),
            ..CreatedEvent::default()
        }
    }

    fn config(scan_url: Option<String>) -> NodeConfig {
        let mut config = NodeConfig::default();
        config.canton.scan_url = scan_url;
        config
    }

    async fn scan() -> MockServer {
        let scan = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/scan/v0/open-and-issuing-mining-rounds"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ROUNDS))
            .mount(&scan)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/scan/v0/featured-apps/by-contract-id/00far"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FEATURED_APP_RIGHT))
            .mount(&scan)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/scan/v0/featured-apps/by-contract-id/00gone"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&scan)
            .await;
        scan
    }

    /// Disclosures for `created` when the request already carries the
    /// AmuletRules, so no test reaches the public DSO API.
    async fn disclosures(
        created: &CreatedEvent,
        scan_url: Option<String>,
    ) -> Result<Vec<DisclosedContract>> {
        let pins = PaidAcceptPins::of(created)?;
        let requested = HashSet::from(["00rules"]);
        paid_accept_disclosures(
            &reqwest::Client::new(),
            &config(scan_url),
            &pins,
            &requested,
        )
        .await
    }

    #[test]
    fn pins_are_read_from_the_app_transfer_context() -> Result<()> {
        let pins = PaidAcceptPins::of(&proposal("00round", Some("00far")))?;
        assert_eq!(pins.amulet_rules, "00rules");
        assert_eq!(pins.open_mining_round, "00round");
        assert_eq!(pins.featured_app_right.as_deref(), Some("00far"));

        let pins = PaidAcceptPins::of(&proposal("00round", None))?;
        assert_eq!(pins.featured_app_right, None);
        Ok(())
    }

    #[test]
    fn a_proposal_without_a_transfer_context_is_an_error() {
        let err = PaidAcceptPins::of(&CreatedEvent::default()).expect_err("nothing to read");
        assert!(format!("{err:#}").contains("appTransferContext"));
    }

    #[tokio::test]
    async fn the_pinned_round_and_featured_app_right_come_from_scan() -> Result<()> {
        let scan = scan().await;
        let round = round_66344();

        let disclosed = disclosures(
            &proposal(&round.contract_id, Some("00far")),
            Some(format!("{}/api/scan", scan.uri())),
        )
        .await?;

        let cids: Vec<&str> = disclosed.iter().map(|d| d.contract_id.as_str()).collect();
        assert_eq!(cids, [round.contract_id.as_str(), "00far"]);
        // The blobs are the decoded `created_event_blob`s scan served.
        assert_eq!(
            disclosed[0].created_event_blob,
            base64::engine::general_purpose::STANDARD.decode(&round.created_event_blob)?
        );
        assert_eq!(disclosed[1].created_event_blob, b"far");
        Ok(())
    }

    #[tokio::test]
    async fn without_a_scan_url_the_accept_is_refused_as_not_configured() {
        let err = disclosures(&proposal(&round_66344().contract_id, None), None)
            .await
            .expect_err("the round must come from scan");

        assert!(err.downcast_ref::<ScanNotConfigured>().is_some(), "{err:#}");
        let (status, _) = execute_failure(&err);
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn without_a_scan_url_a_request_that_discloses_everything_still_executes() -> Result<()> {
        let pins = PaidAcceptPins::of(&proposal("00round", Some("00far")))?;
        let requested = HashSet::from(["00rules", "00round", "00far"]);

        let disclosed =
            paid_accept_disclosures(&reqwest::Client::new(), &config(None), &pins, &requested)
                .await?;

        assert!(disclosed.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_pinned_round_scan_no_longer_lists_has_closed() {
        let scan = scan().await;

        let err = disclosures(
            &proposal("00closed", None),
            Some(format!("{}/api/scan", scan.uri())),
        )
        .await
        .expect_err("a closed round cannot be disclosed");

        let message = format!("{err:#}");
        assert!(message.contains("00closed"), "{message}");
        assert!(message.contains("propose again"), "{message}");
    }

    #[tokio::test]
    async fn a_featured_app_right_scan_does_not_list_is_refused() {
        let scan = scan().await;

        let err = disclosures(
            &proposal(&round_66344().contract_id, Some("00gone")),
            Some(format!("{}/api/scan", scan.uri())),
        )
        .await
        .expect_err("an inactive right cannot be disclosed");

        let message = format!("{err:#}");
        assert!(message.contains("00gone"), "{message}");
    }

    #[test]
    fn any_other_execute_failure_stays_a_500() {
        let (status, error) = execute_failure(&anyhow::anyhow!("ledger said no"));

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error,
            "Failed to execute action: ledger said no. See the DecMan server log."
        );
    }

    /// The answer and the audit row name what failed. What the other service
    /// said stays in the server log.
    #[test]
    fn an_execute_failure_names_its_cause_but_not_the_chain() {
        let error = anyhow::anyhow!("the response held no rounds")
            .context("Failed to fetch the open mining rounds from scan");

        let (_, error) = execute_failure(&error);

        assert_eq!(
            error,
            "Failed to execute action: Failed to fetch the open mining rounds from scan. See \
             the DecMan server log."
        );
    }

    /// An HTTP status is not internal, so a scan refusal keeps it. Scan's
    /// body stays in the server log.
    #[test]
    fn a_scan_refusal_keeps_its_status() {
        let error = anyhow::Error::new(ScanRefused {
            status: reqwest::StatusCode::FORBIDDEN,
            body: "access denied for 10.0.0.7".to_string(),
        })
        .context("Failed to fetch the open mining rounds from scan");

        let (status, error) = execute_failure(&error);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error,
            "Failed to execute action: Failed to fetch the open mining rounds from scan (scan \
             answered 403 Forbidden). See the DecMan server log."
        );
    }
}
