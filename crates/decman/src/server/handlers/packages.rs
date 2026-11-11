//! Vetted packages, expected versions and the peer package comparison.

use std::collections::{HashMap, HashSet};

use actix_web::{HttpResponse, Responder, get, web};
use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ListPackagesRequest, package_service_client::PackageServiceClient,
};
use chrono::Utc;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::server::handlers::token_standard::fetch_dso_info;

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, Peer},
    db::schema::SchemaRead,
    error::Result,
    noise::NoiseError,
    server::{
        AppState,
        package_inventory::{TopologyReader, expected_splice_versions, fetch_vetted_packages},
        types::{
            ErrorResponse, ExpectedVersionsResponse, PackageInfo, PeerErrorKind,
            PeerPackageComparison, PeerPackageResult, VettedPackageInfo,
        },
    },
};

#[utoipa::path(
    tag = "Packages",
    responses(
        (status = 200, description = "Vetted packages", body = Vec<VettedPackageInfo>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
/// Get vetted packages for this participant
#[get("/packages/vetted")]
pub async fn get_vetted_packages(data: web::Data<AppState>) -> impl Responder {
    // Reads topology vetting state. Neither list contains the other: a DAR can
    // be uploaded without being vetted, and a vetting can outlive its DAR
    // (e.g. after a restore from backup).
    match fetch_vetted_packages(&data.config).await {
        Ok(packages) => HttpResponse::Ok().json(packages),
        Err(e) => {
            tracing::error!("Failed to list vetted packages: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to list vetted packages: {e}"),
            })
        }
    }
}

/// Expected versions for the package families that have a trusted source
///
/// Only Splice packages have one: the DSO's AmuletRules pins the version of
/// each Splice package the network runs. Every other package has no expected
/// version, and the caller compares observed versions only.
#[utoipa::path(
    tag = "Packages",
    responses(
        (status = 200, description = "Expected package versions", body = ExpectedVersionsResponse),
        (status = 502, description = "The DSO API could not be read", body = ErrorResponse)
    )
)]
#[get("/packages/expected-versions")]
pub async fn get_expected_versions(data: web::Data<AppState>) -> impl Responder {
    let now = Utc::now();
    let packages = match fetch_dso_info(&data.http_client, &data.config).await {
        Ok(dso) => expected_splice_versions(&dso, now),
        Err(e) => Err(e),
    };
    match packages {
        Ok(packages) => HttpResponse::Ok().json(ExpectedVersionsResponse {
            source: "DSO AmuletRules".to_string(),
            fetched_at: now.timestamp(),
            packages,
        }),
        Err(e) => {
            tracing::warn!("Failed to read expected package versions: {e:#}");
            HttpResponse::BadGateway().json(ErrorResponse {
                error: format!("Failed to read expected package versions: {e}"),
            })
        }
    }
}

/// Query parameters for the peer package comparison
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ComparePeersQuery {
    /// Comma-separated participant ids to compare against. Omitted means every
    /// configured peer. A participant need not be a configured peer: its
    /// vetting is read from the synchronizer, which holds every participant's.
    #[serde(default)]
    pub participants: Option<String>,
}

/// Compare this node's packages with each peer's, read from the synchronizer
#[utoipa::path(
    tag = "Packages",
    params(ComparePeersQuery),
    responses(
        (status = 200, description = "Peer package comparison", body = PeerPackageComparison),
        (status = 400, description = "A participant id does not parse", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/packages/compare-peers")]
pub async fn compare_peer_packages(
    data: web::Data<AppState>,
    query: web::Query<ComparePeersQuery>,
) -> impl Responder {
    let selection = match query.participants.as_deref().map(parse_participant_list) {
        None => None,
        Some(Ok(ids)) => Some(ids),
        Some(Err(e)) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid participants: {e}"),
            });
        }
    };
    match fetch_peer_packages(&data.config, &data.db, selection).await {
        Ok(comparison) => HttpResponse::Ok().json(comparison),
        Err(e) => {
            tracing::error!("Failed to compare peer packages: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to compare peer packages: {e}"),
            })
        }
    }
}

/// What an operator should check, for a Noise request to a peer that failed.
///
/// The raw error names a symptom — `snow error: input error`,
/// `Message too short: got 0` — and an operator reading it has no way to get
/// from there to an action. Each arm below names the thing to go and look at.
///
/// The match is exhaustive (no wildcard), so a new `NoiseError` variant must
/// be given a hint rather than silently inheriting a vague one.
pub(super) fn peer_failure_hint(err: &NoiseError) -> &'static str {
    match err {
        NoiseError::TcpConnectionTimeout(_) | NoiseError::TcpConnectionFailed(_) => {
            "nothing is accepting connections on that address — check the peer is up and that \
             its advertised host/port reach it from here"
        }
        NoiseError::RequestTimeout => {
            "connected, but the peer never answered — it may be overloaded, or wedged mid-request"
        }
        NoiseError::Noise(_) | NoiseError::HandshakeFailed | NoiseError::DecryptionError => {
            "the Noise handshake failed — this node's key or the derived PSK does not match what \
             the peer expects, so check each side has the other's current public key"
        }
        NoiseError::WireVersionMismatch { .. } => {
            "the peer speaks a different decman wire protocol, so the key is not the problem — \
             the error names both versions; upgrade whichever side is older"
        }
        NoiseError::BadStatusCode(..) => {
            "the peer answered but refused the request — it may not have this node registered as \
             a peer, or a load balancer in front of it has no healthy backend"
        }
        NoiseError::InvalidMessage => {
            "the peer answered with something this build cannot parse — most often an empty body \
             from a denied request or an unhealthy proxy, or a peer on an older wire format"
        }
        NoiseError::JsonSerialization(_) => {
            "the peer's payload did not deserialize — likely a version skew between the two builds"
        }
        NoiseError::Io(_) | NoiseError::Hyper(_) => {
            "the connection broke mid-request — check for a proxy or firewall closing idle \
             connections between the two nodes"
        }
        NoiseError::Http(_)
        | NoiseError::InvalidUri(_)
        | NoiseError::UriParsingError(_)
        | NoiseError::UnknownPeer(_)
        | NoiseError::Anyhow(_) => {
            "check this peer's address, port and public key in the peers table"
        }
    }
}

/// The local package list, and each peer's, for the comparison table.
///
/// The two sides do not measure the same thing. `local_packages` comes from
/// this participant's `PackageService`, so it is what this node has UPLOADED.
/// Each peer's list comes from the topology store, so it is what that peer has
/// VETTED. A package uploaded here but not yet vetted here therefore renders as
/// a mismatch against every peer, because no peer can have vetted it either.
///
/// Reading the local side through `fetch_vetted_packages` would remove that
/// asymmetry, at the cost of dropping uploaded-but-unvetted packages from the
/// operator's table entirely. That is a UI decision, so it is left out of the
/// change that moved the peer side onto the topology store.
/// The distinct participant ids in a comma-separated list, in list order.
fn parse_participant_list(raw: &str) -> Result<Vec<CantonId>> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let id = CantonId::parse(part)?;
        if seen.insert(id.to_string()) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Who to compare against: the `selection` when given, else every configured
/// peer. This node is left out, as its own list is the reference. A selected
/// participant that is not a configured peer has no name to show.
fn comparison_targets(
    peers: &[Peer],
    selection: Option<Vec<CantonId>>,
    current: &CantonId,
) -> Vec<(CantonId, String)> {
    let targets: Vec<(CantonId, String)> = match selection {
        None => peers
            .iter()
            .map(|p| (p.participant_id.clone(), p.name.clone()))
            .collect(),
        Some(ids) => ids
            .into_iter()
            .map(|id| {
                let name = peers
                    .iter()
                    .find(|p| p.participant_id == id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                (id, name)
            })
            .collect(),
    };
    targets
        .into_iter()
        .filter(|(id, _)| id != current)
        .collect()
}

async fn fetch_peer_packages(
    config: &NodeConfig,
    db: &SqlitePool,
    selection: Option<Vec<CantonId>>,
) -> Result<PeerPackageComparison> {
    let mut client = PackageServiceClient::new(config.admin_channel().await?);
    let local_response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await?
        .into_inner();

    let local_packages: Vec<PackageInfo> = local_response
        .package_descriptions
        .into_iter()
        .map(|p| PackageInfo {
            package_id: p.package_id,
            name: p.name,
            version: p.version,
        })
        .collect();

    let local_index: HashMap<&str, &PackageInfo> = local_packages
        .iter()
        .map(|p| (p.package_id.as_str(), p))
        .collect();

    let peers = db.get_all_peers().await?;
    let targets = comparison_targets(&peers, selection, config.participant_id());

    // One reader for the whole set: the synchronizer id and the admin channel
    // are resolved once, not once per peer. The reads stay sequential on
    // purpose — this endpoint is an operator's button click over a handful of
    // peers, and each read is local.
    let mut reader = TopologyReader::connect(config).await?;
    let mut results = Vec::with_capacity(targets.len());
    for (participant_id, name) in targets {
        let read = reader.vetted_package_ids(&participant_id).await;
        results.push(peer_package_result(
            &participant_id,
            name,
            read,
            &local_index,
        ));
    }

    Ok(PeerPackageComparison {
        local_packages,
        peers: results,
    })
}

/// One comparison row, from this node's topology read of `participant_id`.
///
/// `reachable` keeps its wire meaning — "this node has that peer's package
/// list" — but nothing is reached any more. A peer whose read succeeds and
/// names nothing is reported as `NoVettedPackages`: an empty vetting set means
/// the peer cannot run any Daml, which an operator must act on, so it is not
/// reported as a healthy peer holding zero packages.
fn peer_package_result(
    participant_id: &CantonId,
    name: String,
    read: Result<Vec<String>>,
    local_index: &HashMap<&str, &PackageInfo>,
) -> PeerPackageResult {
    let participant_id = participant_id.to_string();
    match read {
        Ok(ids) if ids.is_empty() => PeerPackageResult {
            participant_id,
            name,
            reachable: false,
            error_kind: Some(PeerErrorKind::NoVettedPackages),
            packages: vec![],
        },
        Ok(ids) => {
            // A package this node does not hold has no description to join, so
            // it shows with its id and empty name/version.
            let mut packages: Vec<PackageInfo> = ids
                .into_iter()
                .map(|id| match local_index.get(id.as_str()) {
                    Some(local) => (*local).clone(),
                    None => PackageInfo {
                        package_id: id,
                        name: String::new(),
                        version: String::new(),
                    },
                })
                .collect();
            packages.sort_by(|a, b| {
                (&a.name, &a.version, &a.package_id).cmp(&(&b.name, &b.version, &b.package_id))
            });
            PeerPackageResult {
                participant_id,
                name,
                reachable: true,
                error_kind: None,
                packages,
            }
        }
        Err(e) => {
            tracing::warn!(
                peer = %participant_id,
                error = %format!("{e:#}"),
                "vetted-package read failed"
            );
            PeerPackageResult {
                participant_id,
                name,
                reachable: false,
                error_kind: Some(PeerErrorKind::TopologyReadFailed),
                packages: vec![],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;

    fn test_peer(name: &str) -> anyhow::Result<Peer> {
        Ok(Peer {
            participant_id: CantonId::parse(
                "participant::12200ad4539c269a7b13af6806fb2ee326e7c0d7233fa6144004c416502a2c73fb0b",
            )?,
            name: name.to_string(),
            address: "unused".to_string(),
            port: 0,
            public_key: String::new(),
            party: None,
        })
    }

    #[test]
    fn a_peers_vetted_ids_join_the_local_names() -> anyhow::Result<()> {
        let local = PackageInfo {
            package_id: "pkg-a".to_string(),
            name: "governance-core".to_string(),
            version: "1.0.0".to_string(),
        };
        let index: HashMap<&str, &PackageInfo> = [("pkg-a", &local)].into_iter().collect();

        let got = peer_package_result(
            &test_peer("p2")?.participant_id,
            "p2".to_string(),
            Ok(vec!["pkg-a".to_string(), "pkg-unknown".to_string()]),
            &index,
        );

        assert!(got.reachable);
        assert_eq!(got.error_kind, None);
        // A package this node holds carries its name; one it does not hold
        // still appears, by id, so the operator sees the difference.
        assert_eq!(got.packages.len(), 2);
        let named: Vec<&str> = got.packages.iter().map(|p| p.name.as_str()).collect();
        assert!(named.contains(&"governance-core"), "{named:?}");
        let ids: Vec<&str> = got.packages.iter().map(|p| p.package_id.as_str()).collect();
        assert!(ids.contains(&"pkg-unknown"), "{ids:?}");
        Ok(())
    }

    #[test]
    fn a_peer_that_has_vetted_nothing_is_not_reported_healthy() -> anyhow::Result<()> {
        // An empty vetting set means the peer can run no Daml at all. That is
        // an operator problem, not a peer holding zero packages.
        let index: HashMap<&str, &PackageInfo> = HashMap::new();
        let got = peer_package_result(
            &test_peer("p2")?.participant_id,
            "p2".to_string(),
            Ok(vec![]),
            &index,
        );

        assert!(!got.reachable);
        assert_eq!(got.error_kind, Some(PeerErrorKind::NoVettedPackages));
        assert!(got.packages.is_empty());
        Ok(())
    }

    #[test]
    fn a_failed_topology_read_names_itself() -> anyhow::Result<()> {
        let index: HashMap<&str, &PackageInfo> = HashMap::new();
        let got = peer_package_result(
            &test_peer("p2")?.participant_id,
            "p2".to_string(),
            Err(anyhow::anyhow!("synchronizer unreachable")),
            &index,
        );

        assert!(!got.reachable);
        assert_eq!(got.error_kind, Some(PeerErrorKind::TopologyReadFailed));
        assert!(got.packages.is_empty());
        Ok(())
    }

    const OTHER_PARTICIPANT: &str =
        "participant-9::12200ad4539c269a7b13af6806fb2ee326e7c0d7233fa6144004c416502a2c73fb0b";
    const SELF_PARTICIPANT: &str =
        "self::12200ad4539c269a7b13af6806fb2ee326e7c0d7233fa6144004c416502a2c73fb0b";

    #[test]
    fn a_participant_list_parses_trims_and_dedups() -> anyhow::Result<()> {
        let peer = test_peer("p2")?.participant_id.to_string();
        let raw = format!(" {peer}, {OTHER_PARTICIPANT},,{peer} ");

        let ids = parse_participant_list(&raw)?;

        let got: Vec<String> = ids.iter().map(ToString::to_string).collect();
        assert_eq!(got, vec![peer, OTHER_PARTICIPANT.to_string()]);
        Ok(())
    }

    #[test]
    fn a_malformed_participant_is_rejected() {
        assert!(parse_participant_list("not-a-participant").is_err());
    }

    #[test]
    fn no_selection_compares_every_peer_but_this_node() -> anyhow::Result<()> {
        let peer = test_peer("p2")?;
        let mut self_row = test_peer("me")?;
        self_row.participant_id = CantonId::parse(SELF_PARTICIPANT)?;
        let current = self_row.participant_id.clone();

        let targets = comparison_targets(&[peer.clone(), self_row], None, &current);

        assert_eq!(targets, vec![(peer.participant_id, "p2".to_string())]);
        Ok(())
    }

    #[test]
    fn a_selection_names_known_peers_and_keeps_unknown_participants() -> anyhow::Result<()> {
        // A party can be hosted on a participant this node has no peer row
        // for. Its vetting is still on the synchronizer, so it stays in the
        // comparison, without a name.
        let peer = test_peer("p2")?;
        let other = CantonId::parse(OTHER_PARTICIPANT)?;
        let current = CantonId::parse(SELF_PARTICIPANT)?;
        let peer_id = peer.participant_id.clone();
        let selection = vec![other.clone(), current.clone(), peer_id.clone()];

        let targets = comparison_targets(&[peer], Some(selection), &current);

        assert_eq!(
            targets,
            vec![(other, String::new()), (peer_id, "p2".to_string())]
        );
        Ok(())
    }

    #[test]
    fn peer_packages_sort_stably() -> anyhow::Result<()> {
        // The UI renders this list directly, so two reads of an unchanged
        // peer must not reorder it. Topology order is not stable.
        let index: HashMap<&str, &PackageInfo> = HashMap::new();
        let ids = vec![
            "pkg-c".to_string(),
            "pkg-a".to_string(),
            "pkg-b".to_string(),
        ];
        let got = peer_package_result(
            &test_peer("p2")?.participant_id,
            "p2".to_string(),
            Ok(ids),
            &index,
        );

        let order: Vec<&str> = got.packages.iter().map(|p| p.package_id.as_str()).collect();
        assert_eq!(order, vec!["pkg-a", "pkg-b", "pkg-c"]);
        Ok(())
    }

    /// Every hint must name something to go and look at. The failure this
    /// guards is a new `NoiseError` variant being handed a vague catch-all,
    /// which is how the logs got unactionable in the first place (#332).
    #[test]
    fn every_peer_failure_hint_is_actionable() {
        let cases = [
            NoiseError::TcpConnectionFailed("x".into()),
            NoiseError::TcpConnectionTimeout("x".into()),
            NoiseError::RequestTimeout,
            NoiseError::HandshakeFailed,
            NoiseError::DecryptionError,
            NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, None),
            NoiseError::WireVersionMismatch { peer: 0xD2 },
            NoiseError::InvalidMessage,
            NoiseError::UnknownPeer("x".into()),
        ];
        for e in &cases {
            let hint = peer_failure_hint(e);
            assert!(!hint.is_empty(), "no hint for {e:?}");
            // "check", "the peer", an instruction of some kind — not a restatement.
            assert!(
                hint.len() > 30,
                "hint for {e:?} is too terse to act on: {hint}"
            );
        }

        // The two that used to be indistinguishable must not read alike.
        assert_ne!(
            peer_failure_hint(&NoiseError::InvalidMessage),
            peer_failure_hint(&NoiseError::TcpConnectionFailed("x".into())),
        );
        // A version mismatch must not send the operator to check the key.
        let mismatch = peer_failure_hint(&NoiseError::WireVersionMismatch { peer: 0xD2 });
        assert_ne!(mismatch, peer_failure_hint(&NoiseError::HandshakeFailed));
        assert!(
            mismatch.contains("protocol") && mismatch.contains("upgrade"),
            "the mismatch hint does not name the fix: {mismatch}"
        );
        assert!(
            !mismatch.contains("public key"),
            "the mismatch hint points at the key: {mismatch}"
        );
    }
}
