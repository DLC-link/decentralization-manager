//! Owner keys requested from peers, and the per-peer failure log.

use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex},
    time::Duration,
};

use sqlx::SqlitePool;

use super::owner_claims::supplement_owner_keys_from_topology;
use crate::{
    canton_id::{CantonId, Fingerprint},
    config::{NodeConfig, Peer},
    db::{
        rows::OwnerKeyRefusal,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    noise::{Message, MessageType, NoiseError, NoiseKeypair, parse_public_key, send_noise_message},
    server::{handlers::packages::peer_failure_hint, types::DecentralizedParty},
};

/// A peer reported an owner key that this node kept off its records. Either
/// a peer reports a key that is not its own, or a host changed its key while
/// the old one is still an owner. Both need an operator to look.
fn warn_owner_key_refusal(peer: &str, party_id: &str, owner_key: &str, refusal: &OwnerKeyRefusal) {
    match refusal {
        OwnerKeyRefusal::HeldBy(holder) => tracing::warn!(
            "{peer} reports owner key {owner_key} of {party_id}, which this node has on record \
             for {holder}; the record stays with {holder}"
        ),
        OwnerKeyRefusal::KeepsKey(kept) => tracing::warn!(
            "{peer} reports owner key {owner_key} of {party_id}, but this node has {kept} on \
             record for {peer} and {kept} is still an owner; the record stays {kept}"
        ),
    }
}

/// Query each peer via Noise for their owner keys, then update the DB
pub async fn resolve_owner_keys_from_peers(
    config: &NodeConfig,
    db: &SqlitePool,
    parties: &[DecentralizedParty],
) {
    tracing::debug!("Resolving owner keys from peers...");

    let peers = match db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to load peers for owner key resolution: {e}");
            return;
        }
    };

    let keypair = match NoiseKeypair::from_file(&config.key_file_path()).await {
        Ok(kp) => kp,
        Err(e) => {
            tracing::warn!("Failed to load keypair for owner key resolution: {e}");
            return;
        }
    };

    let current_participant_id = config.participant_id().to_string();
    let known_party_ids: HashSet<String> = parties.iter().map(|p| p.party_id.to_string()).collect();

    forget_removed_peers(&peers);

    for peer in &peers {
        let peer_uid = peer.participant_id.to_string();
        if peer_uid == current_participant_id || peer.public_key.is_empty() {
            continue;
        }

        // Tell the peer which parties we want owner_keys for. See #149: peer
        // used to enumerate the whole synchronizer to build a namespace→party
        // map; we now pass the namespaces (via the full party_ids) directly so
        // the peer can skip that scan.
        let request_payload = match serde_json::to_vec(
            &parties
                .iter()
                .map(|p| p.party_id.to_string())
                .collect::<Vec<_>>(),
        ) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Failed to serialise RequestOwnerKeys payload: {e}");
                continue;
            }
        };

        let outcome =
            request_owner_keys(peer, &keypair, &current_participant_id, request_payload).await;
        report_owner_keys_outcome(
            &peer_uid,
            &format!("{}:{}", peer.address, peer.port),
            outcome.as_ref().err(),
        );
        let Ok(entries) = outcome else {
            continue;
        };

        tracing::debug!(
            "Received {} owner key entries from {peer_uid}",
            entries.len()
        );

        // Update DB with the owner keys
        if let Ok(mut tx) = db.begin_transaction().await {
            for entry in &entries {
                let Some(party_id) = entry["party_id"].as_str() else {
                    continue;
                };
                let Some(owner_key) = entry["owner_key"].as_str() else {
                    continue;
                };

                if !known_party_ids.contains(party_id) {
                    continue;
                }
                let Ok(party_id_canton) = CantonId::parse(party_id) else {
                    tracing::debug!(
                        "Skipping owner-key update from {peer_uid}: bad party_id {party_id}"
                    );
                    continue;
                };
                let mut refusal = match tx
                    .update_participant_owner_key(&party_id_canton, &peer_uid, owner_key)
                    .await
                {
                    Ok(refusal) => refusal,
                    Err(e) => {
                        tracing::debug!("Failed to update owner key for {peer_uid}: {e}");
                        None
                    }
                };
                // A peer that owns the namespace without hosting the party has
                // no participant row, so the claim also goes on the owner.
                match tx
                    .update_owner_claim(&party_id_canton, owner_key, &peer_uid)
                    .await
                {
                    Ok(claim_refusal) => refusal = refusal.or(claim_refusal),
                    Err(e) => {
                        tracing::debug!("Failed to record the owner claim of {peer_uid}: {e}");
                    }
                }
                if let Some(refusal) = &refusal {
                    warn_owner_key_refusal(&peer_uid, party_id, owner_key, refusal);
                }

                // Absent from peers that predate the field. Nothing else can
                // supply it — the party's signing keys carry no owner — so a
                // kick coordinated here falls back to elimination until this
                // peer answers a later refresh.
                let Some(signing_key) = entry["signing_key"].as_str() else {
                    continue;
                };
                let signing_key = match signing_key.parse::<Fingerprint>() {
                    Ok(fingerprint) => fingerprint,
                    Err(e) => {
                        tracing::warn!(
                            "Ignoring signing key from {peer_uid} for {party_id}: {e:#}"
                        );
                        continue;
                    }
                };
                if let Err(e) = tx
                    .update_participant_signing_key(&party_id_canton, &peer_uid, &signing_key)
                    .await
                {
                    tracing::debug!("Failed to update signing key for {peer_uid}: {e}");
                }
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::debug!("Failed to commit owner key updates: {e}");
            }
        }
    }

    // Topology fallback for a peer that does not answer over Noise. It reads
    // the namespace delegations that each participant publishes. See
    // `supplement_owner_keys_from_topology` for the owners it can resolve.
    if let Err(e) = supplement_owner_keys_from_topology(config, db, parties).await {
        tracing::debug!("Topology-based owner-key fallback skipped: {e:#}");
    }
}

/// Why one peer did not supply its owner keys. A change of kind is reported
/// again; the same kind on the next refresh is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerKeysFailureKind {
    BadPublicKey,
    Unreachable,
    TimedOut,
    HandshakeFailed,
    WireVersionMismatch,
    Denied,
    ConnectionBroken,
    BadPeerConfig,
    EmptyReply,
    Refused,
    UnexpectedReply,
    UnparsableReply,
    BadPayload,
}

#[derive(Debug)]
struct OwnerKeysFailure {
    kind: OwnerKeysFailureKind,
    message: String,
}

impl OwnerKeysFailure {
    fn new(kind: OwnerKeysFailureKind, message: String) -> Self {
        Self { kind, message }
    }
}

/// Asks one peer for the owner keys of the parties in `request_payload`.
async fn request_owner_keys(
    peer: &Peer,
    keypair: &NoiseKeypair,
    current_participant_id: &str,
    request_payload: Vec<u8>,
) -> std::result::Result<Vec<serde_json::Value>, OwnerKeysFailure> {
    use OwnerKeysFailureKind as Kind;

    let peer_uid = peer.participant_id.to_string();
    let peer_pub_key = parse_public_key(&peer.public_key).map_err(|e| {
        OwnerKeysFailure::new(
            Kind::BadPublicKey,
            format!("Failed to parse public key for {peer_uid}: {e}"),
        )
    })?;

    let psk = keypair.derive_psk(&peer_pub_key);
    let msg = Message::new(MessageType::RequestOwnerKeys, request_payload);

    tracing::debug!("Requesting owner keys from {peer_uid}");
    let response = match tokio::time::timeout(
        Duration::from_secs(10),
        send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            current_participant_id.as_bytes(),
            &msg,
        ),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            return Err(OwnerKeysFailure::new(
                request_failure_kind(&e),
                format!(
                    "RequestOwnerKeys failed: {e} — {hint}",
                    hint = peer_failure_hint(&e)
                ),
            ));
        }
        Err(_) => {
            return Err(OwnerKeysFailure::new(
                Kind::TimedOut,
                format!(
                    "RequestOwnerKeys timed out after 10s — {hint}",
                    hint = peer_failure_hint(&NoiseError::RequestTimeout)
                ),
            ));
        }
    };

    // A 200 with an empty body. The peer accepted the request and then
    // said nothing, which used to surface as "message too short: got 0" and
    // read like a protocol bug in the peer.
    //
    // A denied request is NOT this case: it arrives as a 503, so
    // `send_noise_message` returns `BadStatusCode` above and never reaches
    // here. This arm is a peer that answered 200 with no frame at all, or a
    // proxy that terminated the request and returned an empty 200.
    if response.is_empty() {
        return Err(OwnerKeysFailure::new(
            Kind::EmptyReply,
            format!(
                "RequestOwnerKeys got an empty reply — {hint}",
                hint = peer_failure_hint(&NoiseError::InvalidMessage)
            ),
        ));
    }

    let response_msg = match Message::from_bytes(&response) {
        Ok(m) if m.msg_type == MessageType::OwnerKeys => m,
        Ok(m) if m.msg_type == MessageType::Error => {
            // An Error frame under 200 OK. The listener's own deny paths
            // send that frame with a 503, so those surface as
            // `BadStatusCode(_, Some(reason))` above rather than here. This
            // arm catches a peer that reports the error in the body while
            // still answering 200. Either way, prefer its words to a guess.
            return Err(OwnerKeysFailure::new(
                Kind::Refused,
                format!(
                    "RequestOwnerKeys refused by the peer: {reason}",
                    reason = String::from_utf8_lossy(&m.payload)
                ),
            ));
        }
        Ok(m) => {
            return Err(OwnerKeysFailure::new(
                Kind::UnexpectedReply,
                format!(
                    "RequestOwnerKeys got an unexpected response type {:?} — the peer may be on \
                     a different wire format",
                    m.msg_type
                ),
            ));
        }
        Err(e) => {
            return Err(OwnerKeysFailure::new(
                Kind::UnparsableReply,
                format!(
                    "RequestOwnerKeys reply did not parse: {e} — {hint}",
                    hint = peer_failure_hint(&NoiseError::InvalidMessage)
                ),
            ));
        }
    };

    serde_json::from_slice(&response_msg.payload).map_err(|e| {
        OwnerKeysFailure::new(
            Kind::BadPayload,
            format!("Failed to deserialize owner keys from {peer_uid}: {e}"),
        )
    })
}

/// Groups transport errors the way [`peer_failure_hint`] does, so a change
/// of cause, such as a peer that comes back but denies the request, warns
/// again.
fn request_failure_kind(err: &NoiseError) -> OwnerKeysFailureKind {
    use OwnerKeysFailureKind as Kind;

    match err {
        NoiseError::TcpConnectionTimeout(_) | NoiseError::TcpConnectionFailed(_) => {
            Kind::Unreachable
        }
        NoiseError::RequestTimeout => Kind::TimedOut,
        NoiseError::Noise(_) | NoiseError::HandshakeFailed | NoiseError::DecryptionError => {
            Kind::HandshakeFailed
        }
        NoiseError::WireVersionMismatch { .. } => Kind::WireVersionMismatch,
        NoiseError::BadStatusCode(..) => Kind::Denied,
        NoiseError::InvalidMessage => Kind::UnparsableReply,
        NoiseError::JsonSerialization(_) => Kind::BadPayload,
        NoiseError::Io(_) | NoiseError::Hyper(_) => Kind::ConnectionBroken,
        NoiseError::Http(_)
        | NoiseError::InvalidUri(_)
        | NoiseError::UriParsingError(_)
        | NoiseError::UnknownPeer(_)
        | NoiseError::Anyhow(_) => Kind::BadPeerConfig,
    }
}

/// The last failure kind of each peer whose owner-keys request is failing.
///
/// The parties view refreshes about once a minute while it is open, and each
/// refresh asks every peer again. Without this record, an unreachable peer
/// logged the same warning on every refresh (#332).
static OWNER_KEYS_FAILURES: LazyLock<Mutex<HashMap<String, OwnerKeysFailureKind>>> =
    LazyLock::new(Default::default);

/// How to log one peer's owner-keys outcome.
#[derive(Debug, PartialEq, Eq)]
enum OwnerKeysLog {
    Warn,
    Debug,
    Recovered,
    Quiet,
}

/// Records one peer's outcome in `failures` and decides how to log it. A new
/// or changed failure warns, a repeated one logs at debug, and the first
/// success after a failure logs at info.
fn record_owner_keys_outcome(
    failures: &mut HashMap<String, OwnerKeysFailureKind>,
    peer: &str,
    now: Option<OwnerKeysFailureKind>,
) -> OwnerKeysLog {
    let previous = match now {
        Some(kind) => failures.insert(peer.to_string(), kind),
        None => failures.remove(peer),
    };
    match (previous, now) {
        (previous, Some(kind)) if previous == Some(kind) => OwnerKeysLog::Debug,
        (_, Some(_)) => OwnerKeysLog::Warn,
        (Some(_), None) => OwnerKeysLog::Recovered,
        (None, None) => OwnerKeysLog::Quiet,
    }
}

fn report_owner_keys_outcome(peer: &str, endpoint: &str, failure: Option<&OwnerKeysFailure>) {
    let log = record_owner_keys_outcome(
        &mut OWNER_KEYS_FAILURES
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
        peer,
        failure.map(|f| f.kind),
    );
    match (log, failure) {
        (OwnerKeysLog::Warn, Some(f)) => tracing::warn!(peer, endpoint, "{}", f.message),
        (OwnerKeysLog::Debug, Some(f)) => tracing::debug!(peer, endpoint, "{}", f.message),
        (OwnerKeysLog::Recovered, _) => {
            tracing::info!(peer, endpoint, "RequestOwnerKeys answered again")
        }
        _ => {}
    }
}

/// Drops the record of peers that are no longer configured.
fn forget_removed_peers(peers: &[Peer]) {
    let listed: HashSet<String> = peers.iter().map(|p| p.participant_id.to_string()).collect();
    OWNER_KEYS_FAILURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|peer, _| listed.contains(peer));
}

#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;

    /// The parties view asks every peer again about once a minute. A peer
    /// that keeps failing must warn once, not on every refresh (#332).
    #[test]
    fn a_failing_peer_warns_once_until_it_recovers() {
        use OwnerKeysFailureKind::{Refused, TimedOut};

        let mut failures = HashMap::new();
        let mut next = |now| record_owner_keys_outcome(&mut failures, "p2", now);

        assert_eq!(next(None), OwnerKeysLog::Quiet);
        assert_eq!(next(Some(TimedOut)), OwnerKeysLog::Warn);
        assert_eq!(next(Some(TimedOut)), OwnerKeysLog::Debug);
        assert_eq!(next(Some(TimedOut)), OwnerKeysLog::Debug);
        assert_eq!(next(None), OwnerKeysLog::Recovered);
        assert_eq!(next(None), OwnerKeysLog::Quiet);
        assert_eq!(next(Some(TimedOut)), OwnerKeysLog::Warn);
        // A new cause is news, even while the peer is still failing.
        assert_eq!(next(Some(Refused)), OwnerKeysLog::Warn);
        assert_eq!(next(Some(Refused)), OwnerKeysLog::Debug);
    }

    /// A peer that comes back but refuses the request has a new problem, and
    /// its reason must reach the operator at warn.
    #[test]
    fn a_new_transport_cause_warns_again() {
        let mut failures = HashMap::new();
        let mut next = |e: &NoiseError| {
            record_owner_keys_outcome(&mut failures, "p2", Some(request_failure_kind(e)))
        };

        let down = NoiseError::TcpConnectionFailed("refused".into());
        let denied = NoiseError::BadStatusCode(
            StatusCode::SERVICE_UNAVAILABLE,
            Some("not a registered peer".into()),
        );
        let mismatch = NoiseError::WireVersionMismatch { peer: 0xD2 };
        assert_eq!(next(&down), OwnerKeysLog::Warn);
        assert_eq!(next(&down), OwnerKeysLog::Debug);
        assert_eq!(next(&denied), OwnerKeysLog::Warn);
        assert_eq!(next(&mismatch), OwnerKeysLog::Warn);
        assert_eq!(next(&mismatch), OwnerKeysLog::Debug);
        assert_eq!(next(&NoiseError::HandshakeFailed), OwnerKeysLog::Warn);
        assert_eq!(next(&NoiseError::DecryptionError), OwnerKeysLog::Debug);
    }

    #[test]
    fn each_peer_keeps_its_own_failure_record() {
        let mut failures = HashMap::new();
        let failed = Some(OwnerKeysFailureKind::EmptyReply);

        assert_eq!(
            record_owner_keys_outcome(&mut failures, "p2", failed),
            OwnerKeysLog::Warn
        );
        assert_eq!(
            record_owner_keys_outcome(&mut failures, "p3", failed),
            OwnerKeysLog::Warn
        );
        assert_eq!(
            record_owner_keys_outcome(&mut failures, "p2", None),
            OwnerKeysLog::Recovered
        );
        assert_eq!(
            record_owner_keys_outcome(&mut failures, "p3", failed),
            OwnerKeysLog::Debug
        );
    }

    /// The peer's own reason reaches the operator through Display, which is
    /// what `{e}` in the fan-out warnings renders.
    #[test]
    fn bad_status_code_surfaces_the_peers_reason() {
        let bare = NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, None);
        let bare_rendered = format!("{bare}");
        assert!(
            bare_rendered.starts_with("Bad status code: 503"),
            "unexpected Display: {bare_rendered}"
        );
        assert!(!bare_rendered.contains("peer said"));

        let with_reason = NoiseError::BadStatusCode(
            StatusCode::SERVICE_UNAVAILABLE,
            Some("request rejected: unknown sender".to_string()),
        );
        let rendered = format!("{with_reason}");
        assert!(
            rendered.contains("unknown sender"),
            "reason missing from Display: {rendered}"
        );
    }
}
