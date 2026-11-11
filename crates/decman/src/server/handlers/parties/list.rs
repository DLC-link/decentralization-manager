//! GET /decentralized-parties and the refresh paths that reuse it.

use std::{collections::HashSet, time::Duration};

use actix_web::{HttpResponse, Responder, get, web};
use canton_proto_rs::com::digitalasset::canton::crypto::admin::v30::vault_service_client::VaultServiceClient;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    party_discovery::my_namespace_fingerprints,
    server::{
        AppState,
        types::{DecentralizedPartiesResponse, DecentralizedParty, ErrorResponse, ResponseSource},
    },
    utils,
};

use super::{
    cache::load_cached_parties,
    discovery::{
        Discovery, DiscoveryGate, MAX_PREFIX_LEN, SINGLE_FLIGHT_WAIT, discover_and_cache,
        discovery_completed_since, is_within_ttl, now_secs,
    },
    owner_keys::resolve_owner_keys_from_peers,
};

/// Query parameters for decentralized parties endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct PartiesQuery {
    /// Filter parties by prefix (e.g., "cbtc-network").
    ///
    /// Parties this node already knows locally (credentials, workflow runs,
    /// cache) that match the prefix suppress discovery. A node that knows
    /// `cbtc::A` and also hosts an unknown `cbtc-v2::B` answers `cb` with A
    /// only. The unfiltered list behaves the same way.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Force a synchronous Canton fetch, bypassing the cache. Used right after
    /// mutating workflows (kick / contracts / dars) so the UI sees fresh data
    /// instead of the up-to-60s-stale cached snapshot.
    #[serde(default)]
    pub refresh: Option<bool>,
    /// Read each party's contracts. Off by default: the read is nine template
    /// ACS page-scans per party, so a whole-deployment list costs memory
    /// proportional to party count (#424). The approvals proposals view asks
    /// for it, one page at a time.
    #[serde(default)]
    pub include_contracts: Option<bool>,
    /// Window into the party list, after a stable sort by party id.
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// How much of each party a read should pull back.
#[derive(Clone, Copy, Default)]
pub struct PartyReadOpts {
    pub include_contracts: bool,
    /// `(offset, limit)`. `None` reads every party this participant hosts.
    pub page: Option<(usize, usize)>,
}

impl PartyReadOpts {
    /// Whether a read of this shape belongs in the cache.
    ///
    /// `dec_parties` holds the whole list without contracts, so only that
    /// shape may be written to it, marked as a completed discovery, or counted
    /// as one. A window or a contract-bearing read would otherwise be served
    /// back later as if it were everything.
    pub(super) fn is_cacheable(&self) -> bool {
        !self.include_contracts && self.page.is_none()
    }
}

/// Get decentralized parties the current participant is a member of
#[utoipa::path(
    tag = "Parties",
    params(PartiesQuery),
    responses(
        (status = 200, description = "Decentralized parties", body = DecentralizedPartiesResponse),
        (status = 400, description = "Prefix too long", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (
            status = 503,
            description = "Too many discoveries in flight; retry",
            body = ErrorResponse
        )
    )
)]
#[get("/decentralized-parties")]
pub async fn get_decentralized_parties(
    data: web::Data<AppState>,
    query: web::Query<PartiesQuery>,
) -> impl Responder {
    let prefix = query.prefix.clone().unwrap_or_default();
    let opts = PartyReadOpts {
        include_contracts: query.include_contracts.unwrap_or(false),
        page: query.limit.map(|limit| (query.offset.unwrap_or(0), limit)),
    };
    // The cache holds the whole-list, contract-free snapshot. A targeted read
    // is neither, so it goes straight to Canton — bounded by its own window.
    let force_refresh =
        query.refresh.unwrap_or(false) || opts.include_contracts || opts.page.is_some();

    if prefix.len() > MAX_PREFIX_LEN {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("prefix must be at most {MAX_PREFIX_LEN} characters"),
        });
    }

    // Try to load from DB cache first (unless caller explicitly demanded fresh)
    let cached = if force_refresh {
        Ok(None)
    } else {
        load_cached_parties(&data.db, &prefix).await
    };
    if let Ok(Some((mut response, updated_at))) = cached {
        response.source = ResponseSource::Cache;

        // Only refresh if the cache is stale. A row stamped ahead of the clock
        // counts as stale too, so a backwards correction cannot pin old rows.
        let is_stale = !is_within_ttl(Some(updated_at), now_secs());

        if is_stale {
            // `discover_and_cache` owns the claim, so a second spawn for the
            // same prefix finds it in flight and does nothing.
            let data = data.clone();
            let prefix = prefix.clone();
            tokio::spawn(async move {
                refresh_and_cache_parties(&data, &prefix).await;
            });
        }

        // A refresh was started above, so say so. Reading the claim set here
        // raced the task that takes it and reported false while a refresh was
        // getting under way.
        response.refreshing = is_stale;

        // Resolve my_owner_key for parties where it's missing (e.g. old cache)
        if response.parties.iter().any(|p| p.my_owner_key.is_none())
            && let Ok(fingerprints) = get_local_namespace_fingerprints(&data.config).await
        {
            for party in &mut response.parties {
                if party.my_owner_key.is_none() {
                    party.my_owner_key = party
                        .owners
                        .iter()
                        .find(|o| fingerprints.contains(o.as_str()))
                        .cloned();
                }
            }
        }

        return HttpResponse::Ok().json(response);
    }

    // No cached rows. A party this node is not a member of and a party that
    // does not exist leave the same empty table, so the cache alone cannot
    // tell "never fetched" from "fetched, found nothing". Answering from a
    // recent completed discovery is what stops a node with no party from
    // re-running that query on every single request.
    let completed_at = data.discovery_completed.read().await.get(&prefix).copied();
    if !force_refresh && is_within_ttl(completed_at, now_secs()) {
        return HttpResponse::Ok().json(DecentralizedPartiesResponse {
            parties: Vec::new(),
            total: 0,
            source: ResponseSource::Cache,
            refreshing: data.refreshing_prefixes.read().await.contains(&prefix),
        });
    }

    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();
    let discovered = discover_and_cache(
        &DiscoveryGate::of(&data),
        &data.config,
        &data.db,
        &prefix,
        auth,
        &party_creds,
        opts,
    )
    .await;

    match discovered {
        Discovery::Done(response) => {
            // Serialise first, then hand the parties themselves to the task.
            // Cloning them instead meant a second full copy of every party and
            // its contracts alive at once, on top of the response body (#415).
            let body = HttpResponse::Ok().json(&response);

            // Owner-key resolution fans out to every peer over Noise, so it
            // stays off the request path. Only for a cacheable read: it writes
            // into the rows that read just cached, and a windowed or
            // contract-bearing request caches nothing, so resolving for it
            // would put a peer fan-out behind every page of the approvals
            // view (#424).
            if opts.is_cacheable() && !response.parties.is_empty() {
                let data = data.clone();
                let parties = response.parties;
                tokio::spawn(async move {
                    resolve_owner_keys_from_peers(&data.config, &data.db, &parties).await;
                });
            }
            body
        }
        Discovery::InFlight => await_in_flight_discovery(&data, &prefix, !force_refresh).await,
        // A discovery finished while this request queued, so the cache holds a
        // fresh answer and even a bypass caller can be served from it.
        Discovery::Superseded => await_in_flight_discovery(&data, &prefix, true).await,
        // Capacity says nothing about this prefix, and an empty list would
        // read as "no parties", so report the overload.
        Discovery::AtCapacity => HttpResponse::ServiceUnavailable().json(ErrorResponse {
            error: "party discovery is at capacity, retry shortly".to_string(),
        }),
        Discovery::Failed(e) => {
            tracing::error!("Failed to fetch decentralized parties: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch decentralized parties: {e}"),
            })
        }
    }
}

/// Answer a request that cannot run its own discovery, because another request
/// holds the prefix or because the concurrency bound is spent.
///
/// Waits [`SINGLE_FLIGHT_WAIT`] for the running discovery to land. The winner
/// records and caches before it releases the prefix, so a prefix that is no
/// longer claimed means its result is already visible here.
///
/// `accept_cached` is false for a caller that asked to bypass the cache. Such a
/// caller may only be served rows the running discovery rewrote, which a moved
/// `updated_at` is the evidence for. Waiting is not evidence on its own: the
/// claim also clears when the discovery fails. When freshness cannot be
/// established it gets a retryable 503, never the rows from before its
/// refresh. A caller that did not ask to bypass takes cached rows gladly, and
/// an empty list only as the last answer, since that reads as "no parties" in a
/// client that does not act on `refreshing`.
async fn await_in_flight_discovery(
    data: &web::Data<AppState>,
    prefix: &str,
    accept_cached: bool,
) -> HttpResponse {
    // A caller that will take cached rows gets them without waiting.
    if accept_cached
        && let Ok(Some((mut response, _))) = load_cached_parties(&data.db, prefix).await
    {
        response.source = ResponseSource::Cache;
        response.refreshing = true;
        return HttpResponse::Ok().json(response);
    }

    // Wait on the in-memory completion count, not on the cache: the running
    // discovery bumps it once, after both the mark and the rows are in place.
    // Re-reading SQLite every tick cost up to thirty queries per waiter.
    let generation_at_entry = data
        .discovery_generations
        .read()
        .await
        .get(prefix)
        .map(|(generation, _)| *generation);
    let deadline = tokio::time::Instant::now() + SINGLE_FLIGHT_WAIT;
    let mut completed = false;

    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;

        let generation_now = data
            .discovery_generations
            .read()
            .await
            .get(prefix)
            .map(|(generation, _)| *generation);
        if discovery_completed_since(generation_at_entry, generation_now) {
            completed = true;
            break;
        }

        // The claim clears on failure too, so this is the end of the wait
        // rather than evidence of a result.
        if !data.refreshing_prefixes.read().await.contains(prefix) {
            break;
        }
    }

    if !accept_cached && !completed {
        // The caller asked to bypass the cache and nothing completed, so the
        // rows here are the ones from before its refresh. Say so instead.
        return HttpResponse::ServiceUnavailable().json(ErrorResponse {
            error: "a refresh for this prefix is still in flight, retry shortly".to_string(),
        });
    }

    let refreshing = data.refreshing_prefixes.read().await.contains(prefix);
    match load_cached_parties(&data.db, prefix).await {
        Ok(Some((mut response, _))) => {
            response.source = ResponseSource::Cache;
            response.refreshing = refreshing;
            HttpResponse::Ok().json(response)
        }
        _ => HttpResponse::Ok().json(DecentralizedPartiesResponse {
            parties: Vec::new(),
            total: 0,
            source: ResponseSource::Cache,
            refreshing,
        }),
    }
}

/// Background task: discover, cache, then resolve owner keys from peers.
async fn refresh_and_cache_parties(data: &web::Data<AppState>, prefix: &str) {
    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();

    match discover_and_cache(
        &DiscoveryGate::of(data),
        &data.config,
        &data.db,
        prefix,
        auth,
        &party_creds,
        PartyReadOpts::default(),
    )
    .await
    {
        Discovery::Done(response) => {
            resolve_owner_keys_from_peers(&data.config, &data.db, &response.parties).await;
        }
        Discovery::InFlight | Discovery::AtCapacity | Discovery::Superseded => {
            tracing::debug!("Skipped background refresh for prefix '{prefix}'");
        }
        Discovery::Failed(e) => {
            tracing::warn!("Background refresh failed for prefix '{prefix}': {e}");
        }
    }
}

/// Refresh one party's cached hosts and owner keys before this node checks a
/// kick of it.
///
/// A peer refuses a kick unless it has every host's owner key on record. Its
/// cache fills only at startup and on a party read, so a party that changed
/// since then would have no keys to check. A discovery already running for
/// the prefix is left to finish, and the keys are resolved for the hosts the
/// cache holds now. Failures are logged: the check that follows refuses the
/// kick if a key is still missing.
pub(crate) async fn refresh_party_for_kick(data: &web::Data<AppState>, party: &CantonId) {
    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();
    let parties = match discover_and_cache(
        &DiscoveryGate::of(data),
        &data.config,
        &data.db,
        &party.prefix,
        auth,
        &party_creds,
        PartyReadOpts::default(),
    )
    .await
    {
        Discovery::Done(response) => response.parties,
        Discovery::Failed(e) => {
            tracing::warn!("Kick accept: refreshing {party} failed: {e:#}; using the cache");
            cached_parties(&data.db, &party.prefix).await
        }
        Discovery::InFlight | Discovery::Superseded | Discovery::AtCapacity => {
            cached_parties(&data.db, &party.prefix).await
        }
    };
    let parties: Vec<DecentralizedParty> = parties
        .into_iter()
        .filter(|cached| cached.party_id == *party)
        .collect();
    if parties.is_empty() {
        tracing::warn!("Kick accept: {party} is not in this node's party cache");
        return;
    }
    resolve_owner_keys_from_peers(&data.config, &data.db, &parties).await;
}

pub(super) async fn cached_parties(db: &SqlitePool, prefix: &str) -> Vec<DecentralizedParty> {
    match load_cached_parties(db, prefix).await {
        Ok(Some((response, _))) => response.parties,
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!("Reading the party cache for prefix '{prefix}' failed: {e:#}");
            Vec::new()
        }
    }
}

/// Query the local participant's vault for namespace key fingerprints.
/// Returns a set of fingerprints that identify this node as an owner.
async fn get_local_namespace_fingerprints(config: &NodeConfig) -> Result<HashSet<String>> {
    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    my_namespace_fingerprints(&mut vault_client).await
}
