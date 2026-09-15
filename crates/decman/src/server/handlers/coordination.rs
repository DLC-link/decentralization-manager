//! Read-only views of the coordination ledger and the ACS handoff endpoints
//! (design D5, D9): `GET /proposals/unsolicited`, `GET /acs-manifests/{party}`,
//! `GET /acs-export/{party}/{target}`, `POST /acs-import/{party}`.
//!
//! The ACS endpoints stream in both directions. A snapshot can be larger than
//! the node's memory, so the handlers hand `onledger::acs` a body stream and
//! return the stream it gives back; nothing here buffers a snapshot.

use actix_web::{HttpRequest, HttpResponse, Responder, get, http::StatusCode, post, web};
use common::coordination::{
    AcsImportResponse, AcsManifestView, AcsManifestsResponse, UnsolicitedProposalsResponse,
};
use serde::Deserialize;

use crate::{
    canton_id::CantonId,
    onledger::{acs, daml::codec::AcsManifestRecord},
    server::{AppState, middleware::require_admin, types::ErrorResponse},
};

const MICROS_PER_SEC: i64 = 1_000_000;

/// Pending topology proposals in the synchronizer store that no accepted
/// `WorkflowProposal` explains. Display only; the observer never signs from
/// this list.
#[utoipa::path(
    tag = "Coordination",
    responses(
        (status = 200, description = "Unsolicited topology proposals", body = UnsolicitedProposalsResponse)
    )
)]
#[get("/proposals/unsolicited")]
pub async fn get_unsolicited_proposals(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(UnsolicitedProposalsResponse {
        proposals: data.onledger.unsolicited_proposals().await,
    })
}

/// The wire view of one manifest. `exported_at` moves from micros to seconds
/// because every other timestamp on the API is in seconds.
fn manifest_view(contract_id: &str, record: &AcsManifestRecord) -> AcsManifestView {
    AcsManifestView {
        contract_id: contract_id.to_string(),
        exporter: record.exporter.clone(),
        exporter_participant: record.exporter_participant.clone(),
        dec_party_id: record.dec_party_id.clone(),
        target_participant: record.target_participant.clone(),
        activation_serial: record.activation_serial,
        size_bytes: record.size_bytes,
        sha256_hex: record.sha256_hex.clone(),
        package_ids: record.package_ids.clone(),
        exported_at: record.exported_at.div_euclid(MICROS_PER_SEC),
    }
}

// The `Err` is the response the handler returns as is, so its size is the
// price of not building it twice.
#[allow(clippy::result_large_err)]
fn parse_id(raw: &str) -> Result<CantonId, HttpResponse> {
    CantonId::parse(raw).map_err(|e| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("invalid Canton id `{raw}`: {e}"),
        })
    })
}

fn no_identity() -> HttpResponse {
    HttpResponse::Conflict().json(ErrorResponse {
        error: "node identity not configured; set one with PUT /node-identity first".to_string(),
    })
}

/// Every active `AcsManifest` for a party that this node can see.
#[utoipa::path(
    tag = "Coordination",
    params(("party" = String, Path, description = "Decentralized party id")),
    responses(
        (status = 200, description = "Manifests", body = AcsManifestsResponse),
        (status = 400, description = "Bad party id", body = ErrorResponse),
        (status = 409, description = "No node identity configured", body = ErrorResponse),
        (status = 500, description = "Ledger read failed", body = ErrorResponse)
    )
)]
#[get("/acs-manifests/{party}")]
pub async fn get_acs_manifests(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party = match parse_id(&path.into_inner()) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let Ok(client) = data.onledger.client().await else {
        return no_identity();
    };
    match acs::read_manifests(&client, &party).await {
        Ok(manifests) => HttpResponse::Ok().json(AcsManifestsResponse {
            manifests: manifests
                .iter()
                .map(|m| manifest_view(&m.contract_id, &m.record))
                .collect(),
        }),
        Err(e) => {
            tracing::error!(error = %e, "GET /acs-manifests failed");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("failed to read manifests: {e}"),
            })
        }
    }
}

/// `?serial=N` of the export endpoint: the activation serial the manifest
/// names.
#[derive(Debug, Deserialize)]
pub struct SerialQuery {
    pub serial: u32,
}

/// Stream the snapshot of `party` for `target` at `serial`: the spool file
/// when this host exported one, else a fresh export from the captured offset.
/// The bytes are what `AcsManifest.sha256Hex` pins; the operator moves the
/// file to the joiner, which verifies it on import.
#[utoipa::path(
    tag = "Coordination",
    params(
        ("party" = String, Path, description = "Decentralized party id"),
        ("target" = String, Path, description = "Participant the snapshot is for"),
        ("serial" = u32, Query, description = "Activation serial")
    ),
    responses(
        (status = 200, description = "gzip-compressed ACS snapshot", content_type = "application/gzip"),
        (status = 400, description = "Bad id", body = ErrorResponse),
        (status = 500, description = "The export could not start", body = ErrorResponse)
    )
)]
#[get("/acs-export/{party}/{target}")]
pub async fn export_acs_snapshot(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<(String, String)>,
    query: web::Query<SerialQuery>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let (party, target) = path.into_inner();
    let party = match parse_id(&party) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let target = match parse_id(&target) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match acs::export_to_response(&data.config, &data.db, &party, &target, query.serial).await {
        Ok(stream) => {
            let filename = acs::spool_path(&data.config, &party, &target, query.serial)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("snapshot.acs.gz")
                .to_string();
            HttpResponse::Ok()
                .content_type("application/gzip")
                .insert_header((
                    actix_web::http::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                ))
                .streaming(stream)
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), %party, %target, "ACS export failed");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("ACS export failed: {e}"),
            })
        }
    }
}

/// `?serial=N&exporter=<participant>` of the import endpoint.
#[derive(Debug, Deserialize)]
pub struct ImportQuery {
    pub serial: u32,
    /// The participant that exported the file; selects its manifest.
    pub exporter: String,
}

/// The HTTP status for an import error, from the words `onledger::acs` uses:
/// a missing manifest is 404, a rejected manifest or a file that does not
/// match it is 400, anything else 500.
fn import_error_status(message: &str) -> StatusCode {
    let lower = message.to_ascii_lowercase();
    if lower.contains("no acsmanifest") || lower.contains("no manifest") {
        StatusCode::NOT_FOUND
    } else if lower.contains("does not match")
        || lower.contains("mismatch")
        || lower.contains("refused")
        || lower.contains("manifest")
    {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// Receive the snapshot an operator moved from a current host and import it
/// (design D9). The body streams straight into the spool directory; the
/// manifest match, the size and hash check, the local vetting check, and the
/// import bracket run in `onledger::acs::import_from_stream`.
#[utoipa::path(
    tag = "Coordination",
    params(
        ("party" = String, Path, description = "Decentralized party id"),
        ("serial" = u32, Query, description = "Activation serial"),
        ("exporter" = String, Query, description = "Participant that exported the snapshot")
    ),
    request_body(content = String, content_type = "application/gzip", description = "The snapshot file"),
    responses(
        (status = 200, description = "Snapshot imported", body = AcsImportResponse),
        (status = 400, description = "Bad id, or the file does not match the manifest", body = ErrorResponse),
        (status = 404, description = "No manifest from that exporter for this participant and serial", body = ErrorResponse),
        (status = 409, description = "No node identity configured", body = ErrorResponse),
        (status = 500, description = "Import failed", body = ErrorResponse)
    )
)]
#[post("/acs-import/{party}")]
pub async fn import_acs_snapshot(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<ImportQuery>,
    payload: web::Payload,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party = match parse_id(&path.into_inner()) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let exporter = match parse_id(&query.exporter) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if data.onledger.identity().await.is_none() {
        return no_identity();
    }
    match acs::import_from_stream(&data.onledger, &party, query.serial, &exporter, payload).await {
        Ok(report) => HttpResponse::Ok().json(AcsImportResponse {
            size_bytes: report.size_bytes,
            sha256_hex: report.sha256_hex,
            manifest_contract_id: report.manifest_cid,
        }),
        Err(e) => {
            let message = format!("{e:#}");
            tracing::error!(error = %message, %party, "ACS import failed");
            HttpResponse::build(import_error_status(&message)).json(ErrorResponse {
                error: format!("ACS import failed: {e}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    #[test]
    fn manifest_view_reports_seconds() {
        let record = AcsManifestRecord {
            exporter: CantonId::parse(&format!("node-p1::{NS}")).expect("id"),
            exporter_participant: "p1".into(),
            observers: vec![],
            dec_party_id: format!("cbtc::{NS}"),
            target_participant: "p4".into(),
            activation_serial: 7,
            size_bytes: 3,
            sha256_hex: "ab".repeat(32),
            package_ids: vec!["pkg".into()],
            exported_at: 1_700_000_000_000_000,
        };
        let view = manifest_view("00m", &record);
        assert_eq!(view.contract_id, "00m");
        assert_eq!(view.exported_at, 1_700_000_000);
        assert_eq!(view.activation_serial, 7);
        assert_eq!(view.exporter_participant, "p1");
        assert_eq!(view.package_ids, vec!["pkg"]);
    }

    #[test]
    fn import_errors_map_to_the_operator_facing_status() {
        assert_eq!(
            import_error_status("no AcsManifest of cbtc from p1 for p4 at serial 7"),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            import_error_status("the uploaded file does not match the manifest: size"),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            import_error_status("manifest refused: exporter is not a head host"),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            import_error_status("ImportPartyAcs: transport error"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn bad_ids_are_a_400() {
        assert!(parse_id("not-a-canton-id").is_err());
        assert!(parse_id(&format!("cbtc::{NS}")).is_ok());
    }
}
