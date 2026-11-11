use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    GetSynchronizerIdRequest,
    synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
};

use crate::{config::NodeConfig, error::Result};

/// How long a resolved physical synchronizer ID is trusted.
///
/// A logical synchronizer upgrade (LSU) moves the participant to a successor
/// physical synchronizer while DecMan keeps running, so the ID can change at
/// any time (#567). Resolving it costs a fresh gRPC channel and a call to the
/// Canton admin API, about 4.3s median over a kubectl-tunneled devnet (#149).
/// Five minutes keeps that cost off most requests and bounds how long a stale
/// ID survives when no error clears it first.
const SYNCHRONIZER_ID_TTL: Duration = Duration::from_secs(300);

/// Canton's error id for a topology store it does not know. A cached
/// synchronizer ID names such a store once an LSU has moved the participant on.
const TOPOLOGY_STORE_NOT_FOUND: &str = "TOPOLOGY_STORE_NOT_FOUND";

/// A resolved physical synchronizer ID and when it was resolved.
#[derive(Debug, Clone)]
struct CachedSynchronizerId {
    id: String,
    resolved_at: Instant,
}

/// Process-wide cache for the resolved physical synchronizer ID.
static SYNCHRONIZER_ID_CACHE: Mutex<Option<CachedSynchronizerId>> = Mutex::new(None);

/// The cached ID, if it is younger than [`SYNCHRONIZER_ID_TTL`] at `now`.
fn fresh_synchronizer_id(
    cache: &Mutex<Option<CachedSynchronizerId>>,
    now: Instant,
) -> Option<String> {
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|cached| now.duration_since(cached.resolved_at) < SYNCHRONIZER_ID_TTL)
        .map(|cached| cached.id.clone())
}

/// Clears `cache` when `error` reports an unknown topology store. Returns
/// whether it cleared anything.
fn forget_synchronizer_id_in(cache: &Mutex<Option<CachedSynchronizerId>>, error: &str) -> bool {
    if !error.contains(TOPOLOGY_STORE_NOT_FOUND) {
        return false;
    }
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .is_some()
}

/// Drops the cached synchronizer ID when `error` reports an unknown topology
/// store, so the next call resolves the successor after an LSU. Workflow step
/// failures pass their error text here before they retry or fail the run.
pub fn forget_synchronizer_id_on_unknown_store(error: &str) {
    if forget_synchronizer_id_in(&SYNCHRONIZER_ID_CACHE, error) {
        tracing::info!(
            "Dropped the cached synchronizer ID: Canton no longer knows its topology store, \
             most likely after a logical synchronizer upgrade"
        );
    }
}

/// Get synchronizer ID from config, cached for [`SYNCHRONIZER_ID_TTL`].
///
/// Queries the participant's synchronizer connectivity service to get the
/// physical synchronizer ID for the configured synchronizer alias. A cached
/// value younger than the TTL is returned without a network round trip.
pub async fn get_synchronizer_id(config: &NodeConfig) -> Result<String> {
    if let Some(id) = fresh_synchronizer_id(&SYNCHRONIZER_ID_CACHE, Instant::now()) {
        return Ok(id);
    }
    let id = fetch_synchronizer_id(config, config.synchronizer()).await?;
    *SYNCHRONIZER_ID_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(CachedSynchronizerId {
        id: id.clone(),
        resolved_at: Instant::now(),
    });
    Ok(id)
}

/// Get the physical synchronizer ID for an alias, over the configured Admin
/// API channel.
pub async fn fetch_synchronizer_id(
    config: &NodeConfig,
    synchronizer_alias: &str,
) -> Result<String> {
    let mut conn_client = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);

    let response = conn_client
        .get_synchronizer_id(tonic::Request::new(GetSynchronizerIdRequest {
            synchronizer_alias: synchronizer_alias.to_string(),
        }))
        .await?
        .into_inner();

    if response.physical_synchronizer_id.is_empty() {
        anyhow::bail!("No synchronizer ID returned for synchronizer alias '{synchronizer_alias}'");
    }

    Ok(response.physical_synchronizer_id)
}

/// Extract synchronizer fingerprint from full synchronizer ID
///
/// Canton 3.4+ returns synchronizer IDs in format: `<alias>::<fingerprint>::<protocol-version>`
/// For party allocation, we need the format `<alias>::<fingerprint>` (removing only the protocol version).
///
/// Example:
/// - Input: `global-domain::122033d02b977e2b698d6a6397eb62e43f7bff34bc8fa814384c4a533d1162239df8::34-0`
/// - Output: `global-domain::122033d02b977e2b698d6a6397eb62e43f7bff34bc8fa814384c4a533d1162239df8`
pub fn extract_synchronizer_fingerprint(synchronizer_id: &str) -> Result<String> {
    let parts: Vec<&str> = synchronizer_id.split("::").collect();

    if parts.len() == 3 {
        // Format: alias::fingerprint::version -> return alias::fingerprint
        Ok(format!(
            "{alias}::{fingerprint}",
            alias = parts[0],
            fingerprint = parts[1]
        ))
    } else if parts.len() == 2 {
        // Already in alias::fingerprint format
        Ok(synchronizer_id.to_string())
    } else {
        anyhow::bail!(
            "Invalid synchronizer ID format '{synchronizer_id}': expected format '<alias>::<fingerprint>::<version>' or '<alias>::<fingerprint>'"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_holding(id: &str, resolved_at: Instant) -> Mutex<Option<CachedSynchronizerId>> {
        Mutex::new(Some(CachedSynchronizerId {
            id: id.to_string(),
            resolved_at,
        }))
    }

    /// The error a peer hit on devnet after the 2026-10-21 LSU (#567).
    const STALE_STORE: &str = "Aborting peer: 6 consecutive step failures: code: 'Some requested \
        entity was not found', message: \"TOPOLOGY_STORE_NOT_FOUND(11,0): Topology store \
        'global-domain::1220be58::35-5' is not known.\"";

    #[test]
    fn a_cached_id_expires_after_the_ttl() {
        let start = Instant::now();
        let cache = cache_holding("global-domain::1220be58::35-5", start);

        assert_eq!(
            fresh_synchronizer_id(&cache, start + SYNCHRONIZER_ID_TTL - Duration::from_secs(1)),
            Some("global-domain::1220be58::35-5".to_string())
        );
        assert_eq!(
            fresh_synchronizer_id(&cache, start + SYNCHRONIZER_ID_TTL),
            None
        );
        assert_eq!(fresh_synchronizer_id(&Mutex::new(None), start), None);
    }

    #[test]
    fn an_unknown_topology_store_drops_the_cached_id() {
        let cache = cache_holding("global-domain::1220be58::35-5", Instant::now());

        assert!(!forget_synchronizer_id_in(
            &cache,
            "TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE"
        ));
        assert!(fresh_synchronizer_id(&cache, Instant::now()).is_some());

        assert!(forget_synchronizer_id_in(&cache, STALE_STORE));
        assert_eq!(fresh_synchronizer_id(&cache, Instant::now()), None);
        // Nothing left to drop, so a second failure reports no change.
        assert!(!forget_synchronizer_id_in(&cache, STALE_STORE));
    }

    #[test]
    fn extract_synchronizer_fingerprint_strips_version() -> Result {
        // Three-segment form: drop only the trailing protocol version.
        assert_eq!(
            extract_synchronizer_fingerprint("global-domain::1220abcd::34-0")?,
            "global-domain::1220abcd"
        );
        // Two-segment form: already alias::fingerprint, returned unchanged.
        assert_eq!(
            extract_synchronizer_fingerprint("global-domain::1220abcd")?,
            "global-domain::1220abcd"
        );
        Ok(())
    }

    #[test]
    fn extract_synchronizer_fingerprint_rejects_bad_segment_count() {
        assert!(extract_synchronizer_fingerprint("noColons").is_err());
        assert!(extract_synchronizer_fingerprint("a::b::c::d").is_err());
    }
}
