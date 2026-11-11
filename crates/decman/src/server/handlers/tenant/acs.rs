//! ACS replication relayed by the wallet: snapshot blocks and import.

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};

use super::audit::{audited_write, record_import_end};
use crate::{
    server::{
        AppState,
        middleware::{charge_tenant_key, tenant_caller},
        tenant_call_audit::TenantEndpoint,
        types::{
            ErrorResponse, TenantAcsBlockResponse, TenantAcsImportRequest, TenantAcsImportResponse,
        },
    },
    workflow::{
        external_party::add_hosts::{read_party_to_participant, replication_target},
        party_replication::{
            ClearOutcome, collect_party_package_ids, open_export_session,
            pipe::{PipeBlock, PipeTrailer},
            relay::{AcceptedBlock, OnImportEnd, ServedBlock},
            request_onboarding_flag_clear,
        },
    },
};

// ============================================================================
// ACS replication, relayed by the wallet
// ============================================================================

/// Serve one block of the party's ACS for `target`, for the wallet to relay.
///
/// Called on a host that already holds the party, AFTER the add-hosts topology
/// is authorized: Canton scopes the snapshot to the joiner's activation, which
/// must exist first. The offset it searches from was captured at prepare time,
/// before the topology moved.
///
/// Block 1 opens a fresh `ExportPartyAcs` stream and this host keeps it open
/// between requests, handing out successive blocks. Nothing is staged: one block
/// is in flight and neither end accumulates, which is what stops the size of the
/// party deciding whether the transfer is possible.
///
/// Forward-only, so there is no resume. The last block served is replayed if
/// asked for again — that makes a single transport retry safe — but anything
/// else is refused, and asking for block 1 again throws the stream away and
/// starts a new export.
#[utoipa::path(
    tag = "Tenant",
    params(
        ("party" = String, Path, description = "Full party id"),
        ("target" = String, Path, description = "Participant the snapshot is for"),
        ("base_serial" = u32, Query, description = "The serial the add-hosts write was pinned to"),
        ("seq" = u64, Query, description = "Block to serve, 1-based")
    ),
    responses(
        (status = 200, description = "One block of the ACS", body = TenantAcsBlockResponse),
        (status = 400, description = "Bad target participant id or seq", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 409, description = "Out of sync with this host's export; restart from block 1. Or this host is leaving the party", body = ErrorResponse),
        (status = 500, description = "Export failed on this host", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/acs/{target}")]
pub async fn tenant_acs_snapshot(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<(String, String)>,
    query: web::Query<AcsBlockQuery>,
) -> impl Responder {
    // The middleware spends no budget here: one ACS copy asks for many blocks.
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    let (party_id, target) = path.into_inner();
    let target = match crate::canton_id::CantonId::parse(&target) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("target is not a valid participant id: {e}"),
            });
        }
    };
    let replication = match replication_target(&party_id, &target, query.base_serial) {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("party_id is not a valid party id: {e}"),
            });
        }
    };
    if query.seq == 0 {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "seq is 1-based; block 0 does not exist".to_string(),
        });
    }

    let served = data
        .relay_sessions
        .serve_block(
            &data.config,
            &data.db,
            &replication,
            query.seq,
            EXPORT_BLOCK_SIZE,
            async || open_export_session(&data.config, &data.db, &replication).await,
        )
        .await;

    let block = match served {
        Ok(ServedBlock::Block(block)) => block,
        Ok(ServedBlock::OutOfSync { expected, asked }) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!(
                    "this host's export is at block {expected}, not {asked}; the export stream \
                     cannot rewind, so restart the transfer from block 1"
                ),
            });
        }
        Ok(ServedBlock::Held) => return relay_held_response(&party_id),
        Err(e) => {
            tracing::error!("tenant acs snapshot: export failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to export the party's ACS; see the host's logs".to_string(),
            });
        }
    };

    // No contracts means no packages to check, and the ledger scan is not free.
    // Only on block 1: the answer does not change mid-transfer, and the scan is
    // a ledger query that would otherwise run once per block.
    //
    // The scan is best-effort on purpose. It reads the party's contracts over
    // the Ledger API, which needs a credential for that party, and a node
    // hosting an *external* party has none — the key belongs to the wallet.
    // Failing the whole export over a preflight that cannot run on this
    // deployment would block replication entirely; instead the response says
    // the preflight is unavailable and the joiner's own import still validates
    // every contract, just after it has disconnected rather than before.
    let (package_ids, package_preflight) = if query.seq != 1 {
        (Vec::new(), true)
    } else {
        match collect_party_package_ids(&data.config, &party_id, None).await {
            Ok(ids) => (ids, true),
            Err(e) => {
                tracing::warn!(
                    "tenant acs snapshot: package preflight unavailable for {party_id} — this \
                     node holds no ledger credential for an external party, so the joiner's \
                     import will validate packages itself after disconnecting: {e:#}"
                );
                (Vec::new(), false)
            }
        }
    };

    let response = match block {
        PipeBlock::Data { seq, bytes } => TenantAcsBlockResponse {
            party_id,
            seq,
            chunk: STANDARD.encode(&bytes),
            end: false,
            total_len: 0,
            sha256: String::new(),
            package_ids,
            package_preflight,
        },
        PipeBlock::End { seq, trailer } => TenantAcsBlockResponse {
            party_id,
            seq,
            chunk: String::new(),
            end: true,
            total_len: trailer.total_len,
            sha256: trailer.sha256,
            package_ids,
            package_preflight,
        },
    };
    HttpResponse::Ok().json(response)
}

/// Take one relayed ACS block on THIS host, importing it straight into Canton.
///
/// The wallet hands over block N; this host feeds it into the `ImportPartyAcs`
/// stream it holds open for the whole transfer, and the call returns once Canton
/// has taken it. Nothing is buffered or staged: the response is the backpressure
/// that paces the wallet, and the size of the party never becomes a disk or
/// memory commitment here.
///
/// Block 1 starts a fresh import. There is no resume — the source's export
/// stream cannot rewind — so a broken transfer restarts from block 1 rather than
/// continuing, exactly as the add-party path does.
///
/// The final block finishes the import and clears the onboarding marker in the
/// same call. The two stay together: a host that imported but stayed marked is
/// not usable, and one that cleared without importing would start confirming
/// transactions it cannot validate.
///
/// **This host is off the synchronizer for the whole transfer.** `ImportPartyAcs`
/// requires it, and the window is now paced by the wallet. A session that goes
/// quiet is reaped, which fails the import and reconnects the participant.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAcsImportRequest,
    responses(
        (status = 200, description = "Block taken; complete says whether the import finished", body = TenantAcsImportResponse),
        (status = 400, description = "Bad base64, bad party id, or missing packages", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized mapping for this party on this host", body = ErrorResponse),
        (status = 409, description = "Out of sync with this host's import; restart from block 1. Or this host is leaving the party", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; only a block that opens a copy, or that no open copy takes, counts. Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "Import failed on this participant", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/import")]
pub async fn tenant_acs_import(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAcsImportRequest>,
) -> impl Responder {
    // The middleware spends no budget here: one ACS copy sends many blocks.
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    // A copy this host admitted must never be refused partway, or the
    // participant stays off the synchronizer until the reaper ends the copy.
    // So the next block of an open copy, the final one included, spends no
    // budget and writes no row here: the copy's teardown records how it ended.
    // Every other block spends budget: block 1, which opens a copy, block 0,
    // which does not exist, a block that fails the checks below, and a block
    // that no open copy takes next. Two requests that carry the same next
    // block both pass, and the import takes one: a wallet that retries a block
    // it saw time out must not get a 429.
    //
    // Block 0 and block 1 can never skip the budget, so they spend it before
    // the checks: a key with no budget left gets its 429 before the node
    // decodes the chunk.
    let opens = body.seq <= 1;
    if opens && let Err(resp) = charge_tenant_key(&data, key.as_ref(), http_req.path()) {
        return resp;
    }
    let checked = check_import_block(&data, &body);
    if !opens {
        let next = match &checked {
            Ok(checked) => {
                data.relay_sessions
                    .is_next_block(&checked.replication.instance_name, body.seq)
                    .await
            }
            Err(_) => false,
        };
        if next {
            return tenant_acs_import_on_this_host(&data, &body, checked, None).await;
        }
        if let Err(resp) = charge_tenant_key(&data, key.as_ref(), http_req.path()) {
            return resp;
        }
    }
    let endpoint = if opens {
        TenantEndpoint::AcsImportStart
    } else if body.end {
        TenantEndpoint::AcsImportEnd
    } else {
        return tenant_acs_import_on_this_host(&data, &body, checked, None).await;
    };
    let on_end = (body.seq == 1).then(|| record_import_end(&data, key.clone(), &body.party_id));
    let body = body.into_inner();
    let party_id = body.party_id.clone();
    audited_write(&data, key, endpoint, party_id, move |data| async move {
        tenant_acs_import_on_this_host(&data, &body, checked, on_end).await
    })
    .await
}

/// A relayed block that passed the checks that need no Canton call.
struct CheckedBlock {
    replication: crate::workflow::party_replication::ReplicationTarget,
    block: PipeBlock,
}

/// Check a relayed block without a Canton call, and decode its chunk once.
/// The error is the message of the 400 that refuses the block.
fn check_import_block(
    data: &AppState,
    body: &TenantAcsImportRequest,
) -> std::result::Result<CheckedBlock, String> {
    if body.seq == 0 {
        return Err("seq is 1-based; block 0 does not exist".to_string());
    }
    let chunk = STANDARD
        .decode(&body.chunk)
        .map_err(|e| format!("chunk is not valid base64: {e}"))?;
    let replication = replication_target(
        &body.party_id,
        data.config.participant_id(),
        body.base_serial,
    )
    .map_err(|e| format!("party_id is not a valid party id: {e}"))?;
    let block = if body.end {
        PipeBlock::End {
            seq: body.seq,
            trailer: PipeTrailer {
                total_len: body.total_len,
                sha256: body.sha256.clone(),
            },
        }
    } else {
        PipeBlock::Data {
            seq: body.seq,
            bytes: chunk,
        }
    };
    Ok(CheckedBlock { replication, block })
}

async fn tenant_acs_import_on_this_host(
    data: &AppState,
    body: &TenantAcsImportRequest,
    checked: std::result::Result<CheckedBlock, String>,
    on_end: Option<OnImportEnd>,
) -> HttpResponse {
    let CheckedBlock { replication, block } = match checked {
        Ok(checked) => checked,
        Err(error) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
    };

    // Before anything disconnects this participant. `import_party_acs` checks
    // packages, drops the synchronizer connections, and only then does Canton
    // reject a party this node does not host — so without this check any holder
    // of the tenant API key can take this node off the synchronizer by relaying
    // a block for an arbitrary party id.
    //
    // Only on block 1: the check is a topology read, and repeating it per block
    // would put a Canton round trip in front of every one.
    if body.seq == 1 {
        match read_party_to_participant(&data.config, &body.party_id).await {
            Ok(Some(current)) => {
                let self_uid = data.config.participant_id().to_string();
                let onboarding_here = current
                    .mapping
                    .participants
                    .iter()
                    .any(|p| p.participant_uid == self_uid && p.onboarding.is_some());
                if !onboarding_here {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: format!(
                            "{party} is not being onboarded onto this participant; an import \
                             would disconnect it from the synchronizer for a replication it is \
                             not part of",
                            party = body.party_id
                        ),
                    });
                }
            }
            Ok(None) => {
                return HttpResponse::NotFound().json(ErrorResponse {
                    error: format!(
                        "No authorized PartyToParticipant for {party}",
                        party = body.party_id
                    ),
                });
            }
            Err(e) => {
                tracing::error!("tenant acs import: topology read failed: {e:#}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "Failed to read the party's topology; see the host's logs".to_string(),
                });
            }
        }
    }

    let accepted = data
        .relay_sessions
        .accept_block(
            &data.config,
            &data.db,
            &replication,
            &body.package_ids,
            block,
            on_end,
        )
        .await;

    let imported = body.total_len > 0 || !body.end || body.seq > 1;
    match accepted {
        Ok(AcceptedBlock::Accepted { seq }) => HttpResponse::Ok().json(TenantAcsImportResponse {
            party_id: body.party_id.clone(),
            seq,
            complete: false,
            imported: false,
            marker_cleared: false,
        }),
        Ok(AcceptedBlock::Held) => relay_held_response(&body.party_id),
        Ok(AcceptedBlock::OutOfSync { expected, asked }) => {
            HttpResponse::Conflict().json(ErrorResponse {
                error: match expected {
                    Some(expected) => format!(
                        "this host is waiting for block {expected}, not {asked}; the import \
                         stream cannot rewind, so restart the transfer from block 1"
                    ),
                    None => format!(
                        "no ACS import is open on this host for {party}, so block {asked} has \
                         nowhere to go; restart the transfer from block 1",
                        party = body.party_id
                    ),
                },
            })
        }
        Ok(AcceptedBlock::Complete) => {
            // Requested, not waited on. Canton schedules the clearance itself
            // when its safe time has not arrived, so holding the request open
            // for it would turn one call into a ten-minute one. The caller polls
            // /status.
            let marker_cleared =
                match request_onboarding_flag_clear(&data.config, &data.db, &replication).await {
                    Ok(ClearOutcome::Cleared) => true,
                    Ok(ClearOutcome::Proposed) => false,
                    Err(e) => {
                        tracing::error!(
                            "tenant acs import: requesting the marker clear failed: {e:#}"
                        );
                        return HttpResponse::InternalServerError().json(ErrorResponse {
                            error: "Imported the ACS, but could not request the marker clear; \
                                    see the host's logs"
                                .to_string(),
                        });
                    }
                };
            HttpResponse::Ok().json(TenantAcsImportResponse {
                party_id: body.party_id.clone(),
                seq: body.seq,
                complete: true,
                imported,
                marker_cleared,
            })
        }
        Err(e) => {
            tracing::error!("tenant acs import: import failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!(
                    "The ACS import failed on this host; the participant has been reconnected \
                     and the transfer must restart from block 1. See the host's logs. ({first})",
                    first = e.to_string().lines().next().unwrap_or("")
                ),
            })
        }
    }
}

/// Block size for draining and re-serving a snapshot.
const EXPORT_BLOCK_SIZE: usize = 1024 * 1024;

// ============================================================================
// Helpers
// ============================================================================

/// `?base_serial=&seq=` on the ACS block endpoint.
#[derive(Debug, serde::Deserialize)]
pub struct AcsBlockQuery {
    /// Required rather than defaulted: it keys this replication, and guessing
    /// it would silently serve another attempt's export.
    pub base_serial: u32,
    /// Which block to serve, 1-based.
    pub seq: u64,
}

/// The 409 for a relay that would start while this host changes the party's
/// hosting.
fn relay_held_response(party_id: &str) -> HttpResponse {
    HttpResponse::Conflict().json(ErrorResponse {
        error: format!(
            "this host is leaving {party_id}, so no ACS relay for it may start; retry once the \
             unhost has finished"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        super::{onboard::tenant_prepare, test_support::test_participant},
        *,
    };

    /// A party id that `replication_target` accepts.
    fn valid_party() -> String {
        format!("alice::1220{}", "aa".repeat(32))
    }

    /// The tenant key `"key"` against a budget of `limit` calls per minute, as
    /// a production node checks it, on a node with a participant id.
    fn meter(mut state: AppState, limit: u32) -> web::Data<AppState> {
        state.test_mode = false;
        state.config.tenant_api_keys = vec!["key".to_string()];
        state.config.tenant_rate_limit_per_minute = limit;
        state.config.node.participant_id =
            crate::canton_id::CantonId::parse(&test_participant()).ok();
        // Nothing listens here, so a Canton call fails at once.
        state.config.canton.admin_api_host = "127.0.0.1".to_string();
        state.config.canton.admin_api_port = 1;
        web::Data::new(state)
    }

    async fn metered_state(limit: u32) -> web::Data<AppState> {
        let Ok(state) = AppState::for_test(None).await else {
            panic!("the test state must build");
        };
        let Ok(state) = std::sync::Arc::try_unwrap(state.into_inner()) else {
            panic!("the freshly built state has no other owner");
        };
        meter(state, limit)
    }

    /// The instance name of the copy of `party` onto this node at serial 1.
    fn instance_of(state: &AppState, party: &str) -> String {
        match replication_target(party, state.config.participant_id(), 1) {
            Ok(target) => target.instance_name,
            Err(e) => panic!("the test party must name a replication: {e}"),
        }
    }

    /// Refused at validation, so it reaches no Canton.
    fn prepare_without_peers() -> actix_web::test::TestRequest {
        actix_web::test::TestRequest::post()
            .uri("/v0/tenant/prepare")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(serde_json::json!({
                "party_hint": "alice",
                "public_key": STANDARD.encode([7u8; 32]),
                "hosting_peers": [],
            }))
    }

    fn import_block(party: &str, seq: u64, end: bool) -> actix_web::test::TestRequest {
        actix_web::test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(serde_json::json!({
                "party_id": party,
                "base_serial": 1,
                "seq": seq,
                "chunk": STANDARD.encode(b"block"),
                "end": end,
            }))
    }

    /// The middleware spends the budget, and the handler must not spend it a
    /// second time.
    #[actix_web::test]
    async fn a_call_is_charged_once() {
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(metered_state(2).await)
                .wrap(crate::server::middleware::AuthMiddleware)
                .service(tenant_prepare),
        )
        .await;

        for i in 0..2 {
            let response =
                actix_web::test::call_service(&app, prepare_without_peers().to_request()).await;
            assert_eq!(
                response.status(),
                actix_web::http::StatusCode::BAD_REQUEST,
                "call {i} is within a limit of 2"
            );
        }

        let third = actix_web::test::call_service(&app, prepare_without_peers().to_request()).await;
        assert_eq!(
            third.status(),
            actix_web::http::StatusCode::TOO_MANY_REQUESTS
        );
        assert!(
            third
                .headers()
                .get(actix_web::http::header::RETRY_AFTER)
                .is_some(),
            "a 429 must say how long to wait"
        );
        let body: serde_json::Value = actix_web::test::read_body_json(third).await;
        assert!(body["error"].is_string(), "{body}");
    }

    /// One ACS copy sends a block per call. The blocks of an open copy must
    /// not spend the budget the other calls share, and must pass when it is
    /// spent.
    #[actix_web::test]
    async fn the_blocks_of_an_open_copy_are_not_metered() {
        let state = metered_state(1).await;
        let party = valid_party();
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(state)
                .wrap(crate::server::middleware::AuthMiddleware)
                .service(tenant_prepare)
                .service(tenant_acs_import),
        )
        .await;

        for seq in 2..=4 {
            let response =
                actix_web::test::call_service(&app, import_block(&party, seq, false).to_request())
                    .await;
            assert_eq!(
                response.status(),
                actix_web::http::StatusCode::OK,
                "block {seq}"
            );
        }
        let prepare =
            actix_web::test::call_service(&app, prepare_without_peers().to_request()).await;
        assert_eq!(
            prepare.status(),
            actix_web::http::StatusCode::BAD_REQUEST,
            "the blocks must have left the whole budget"
        );

        let response =
            actix_web::test::call_service(&app, import_block(&party, 5, false).to_request()).await;
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::OK,
            "a block of an open copy must pass with the budget spent"
        );
    }
}
