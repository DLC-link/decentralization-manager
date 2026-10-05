//! Topology discovery is expensive: Canton's participant filter scans the store.
//! Requests read a shared snapshot. A missing or stale snapshot starts a single
//! background scan, so a node scans only while someone asks for the list.
use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use actix_web::web;
use chrono::{DateTime, Utc};

use super::AppState;
use crate::workflow::external_party::steps::{HostedExternalParty, list_hosted_external_parties};

/// How long a snapshot serves requests before one of them starts the next scan.
const SNAPSHOT_TTL: Duration = Duration::from_secs(5 * 60);

/// How long requests wait after a failed scan before one starts another.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Devnet scans take minutes. Bound a stuck scan, but let it outlive HTTP and
/// proxy deadlines.
const SCAN_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Default)]
pub struct ExternalPartiesCache(Mutex<CacheInner>);

#[derive(Default)]
struct CacheInner {
    /// The result of the last scan that succeeded.
    snapshot: Option<Snapshot>,
    /// Why the latest scan failed. A successful scan clears it.
    error: Option<String>,
    scanning: bool,
    /// The earliest time a request may start the next scan. `None` until a
    /// scan finishes.
    next_scan: Option<Instant>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub parties: Vec<HostedExternalParty>,
    pub fetched_at: DateTime<Utc>,
}

/// What a request sees.
pub enum View {
    /// The first scan has not finished.
    Pending,
    /// The last good snapshot. `refresh_error` says why a later scan failed.
    Ready {
        snapshot: Snapshot,
        refreshing: bool,
        refresh_error: Option<String>,
    },
    /// No scan has succeeded, and the latest one failed.
    Failed(String),
}

/// Read the snapshot, and start a background scan if it is missing or stale.
pub fn read(data: &web::Data<AppState>) -> View {
    let (view, start) = data.external_parties.observe(Instant::now());
    if start {
        tokio::spawn(scan(data.clone()));
    }
    view
}

async fn scan(data: web::Data<AppState>) {
    let started = Instant::now();
    let mut guard = ScanGuard { data, result: None };
    // Never hold the cache lock over RPCs.
    let result = match tokio::time::timeout(
        SCAN_TIMEOUT,
        list_hosted_external_parties(&guard.data.config),
    )
    .await
    {
        Ok(Ok(parties)) => Ok(parties),
        Ok(Err(e)) => Err(format!("Failed to discover external parties: {e:#}")),
        Err(_) => Err(format!(
            "External party discovery timed out after {} minutes.",
            SCAN_TIMEOUT.as_secs() / 60
        )),
    };
    match &result {
        Ok(parties) => tracing::info!(
            parties = parties.len(),
            elapsed = ?started.elapsed(),
            "External party scan finished"
        ),
        Err(error) => {
            tracing::warn!(%error, elapsed = ?started.elapsed(), "External party scan failed")
        }
    }
    guard.result = Some(result);
}

/// Ends the scan when dropped. A scan task that panics or is dropped then
/// still clears `scanning`, so a later request can start the next scan.
struct ScanGuard {
    data: web::Data<AppState>,
    result: Option<Result<Vec<HostedExternalParty>, String>>,
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        let result = self.result.take().unwrap_or_else(|| {
            tracing::warn!("External party scan stopped before it finished");
            Err("External party discovery stopped before it finished.".into())
        });
        self.data.external_parties.finish(result, Instant::now());
    }
}

impl ExternalPartiesCache {
    fn lock(&self) -> MutexGuard<'_, CacheInner> {
        // Nothing panics while holding the lock, so a poisoned state is intact.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Return what a request sees, and whether the caller must start a scan.
    /// The claim happens under the lock, so at most one scan runs at a time.
    fn observe(&self, now: Instant) -> (View, bool) {
        let mut inner = self.lock();
        let start = !inner.scanning && inner.next_scan.is_none_or(|at| now >= at);
        inner.scanning |= start;
        (inner.view(), start)
    }

    fn finish(&self, result: Result<Vec<HostedExternalParty>, String>, now: Instant) {
        let mut inner = self.lock();
        inner.scanning = false;
        match result {
            Ok(parties) => {
                inner.snapshot = Some(Snapshot {
                    parties,
                    fetched_at: Utc::now(),
                });
                inner.error = None;
                inner.next_scan = Some(now + SNAPSHOT_TTL);
            }
            Err(error) => {
                inner.error = Some(error);
                inner.next_scan = Some(now + RETRY_AFTER);
            }
        }
    }
}

impl CacheInner {
    fn view(&self) -> View {
        match (&self.snapshot, &self.error) {
            (Some(snapshot), refresh_error) => View::Ready {
                snapshot: snapshot.clone(),
                refreshing: self.scanning,
                refresh_error: refresh_error.clone(),
            },
            (None, Some(error)) => View::Failed(error.clone()),
            (None, None) => View::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, http::StatusCode};

    const SECOND: Duration = Duration::from_secs(1);

    fn party(party_id: &str) -> HostedExternalParty {
        HostedExternalParty {
            party_id: party_id.into(),
            fingerprint: "key".into(),
            threshold: 1,
            host_count: 1,
            created_at: None,
            onboarding: false,
            hosts: vec![],
        }
    }

    /// The fields of a `Ready` view, with the parties reduced to their ids.
    fn ready(view: View) -> (Vec<String>, DateTime<Utc>, bool, Option<String>) {
        match view {
            View::Ready {
                snapshot,
                refreshing,
                refresh_error,
            } => (
                snapshot.parties.into_iter().map(|p| p.party_id).collect(),
                snapshot.fetched_at,
                refreshing,
                refresh_error,
            ),
            View::Pending => panic!("expected a snapshot, got Pending"),
            View::Failed(error) => panic!("expected a snapshot, got Failed({error})"),
        }
    }

    #[test]
    fn first_request_starts_one_scan_and_reports_pending() {
        let cache = ExternalPartiesCache::default();
        let now = Instant::now();
        let (view, start) = cache.observe(now);
        assert!(start);
        assert!(matches!(view, View::Pending));
        let (view, start) = cache.observe(now + SECOND);
        assert!(!start, "a running scan must not start a second one");
        assert!(matches!(view, View::Pending));
    }

    #[test]
    fn snapshot_serves_until_ttl_then_refreshes_while_still_served() {
        let cache = ExternalPartiesCache::default();
        let t0 = Instant::now();
        cache.observe(t0);
        cache.finish(Ok(vec![party("a::key")]), t0);

        let (view, start) = cache.observe(t0 + SNAPSHOT_TTL - SECOND);
        assert!(!start);
        let (parties, fetched_at, refreshing, refresh_error) = ready(view);
        assert_eq!(parties, ["a::key"]);
        assert!(!refreshing);
        assert_eq!(refresh_error, None);

        let (view, start) = cache.observe(t0 + SNAPSHOT_TTL);
        assert!(start);
        let (parties, stale_fetched_at, refreshing, _) = ready(view);
        assert_eq!(parties, ["a::key"]);
        assert_eq!(stale_fetched_at, fetched_at);
        assert!(refreshing);
    }

    #[test]
    fn failed_refresh_keeps_last_good_snapshot_and_retries_after_backoff() {
        let cache = ExternalPartiesCache::default();
        let t0 = Instant::now();
        cache.observe(t0);
        cache.finish(Ok(vec![party("a::key")]), t0);
        let (_, fetched_at, _, _) = ready(cache.observe(t0).0);

        let t1 = t0 + SNAPSHOT_TTL;
        assert!(cache.observe(t1).1);
        cache.finish(Err("Canton unavailable".into()), t1);
        let (view, start) = cache.observe(t1 + RETRY_AFTER - SECOND);
        assert!(!start);
        let (parties, kept_fetched_at, refreshing, refresh_error) = ready(view);
        assert_eq!(parties, ["a::key"]);
        assert_eq!(kept_fetched_at, fetched_at);
        assert!(!refreshing);
        assert_eq!(refresh_error.as_deref(), Some("Canton unavailable"));

        let t2 = t1 + RETRY_AFTER;
        assert!(cache.observe(t2).1);
        cache.finish(Ok(vec![party("b::key")]), t2);
        let (parties, _, _, refresh_error) = ready(cache.observe(t2).0);
        assert_eq!(parties, ["b::key"]);
        assert_eq!(refresh_error, None);
    }

    #[test]
    fn failure_without_snapshot_stays_failed_while_retrying() {
        let cache = ExternalPartiesCache::default();
        let t0 = Instant::now();
        cache.observe(t0);
        cache.finish(Err("Canton unavailable".into()), t0);

        let (view, start) = cache.observe(t0 + RETRY_AFTER - SECOND);
        assert!(!start);
        assert!(matches!(view, View::Failed(e) if e == "Canton unavailable"));
        // The retry keeps the error visible: switching to Pending would hide
        // it for the whole scan and then show it again.
        let (view, start) = cache.observe(t0 + RETRY_AFTER);
        assert!(start);
        assert!(matches!(view, View::Failed(e) if e == "Canton unavailable"));
    }

    #[actix_web::test]
    async fn scan_that_never_finishes_still_frees_the_next_scan() {
        let state = AppState::for_test(None).await.unwrap();
        let t0 = Instant::now();
        assert!(state.external_parties.observe(t0).1);
        // A panicking or dropped scan task drops its guard without a result.
        drop(ScanGuard {
            data: state.clone(),
            result: None,
        });
        let (view, start) = state.external_parties.observe(Instant::now());
        assert!(!start, "the failure waits out the retry delay");
        assert!(matches!(view, View::Failed(e) if e.contains("stopped")));
        assert!(
            state
                .external_parties
                .observe(Instant::now() + RETRY_AFTER)
                .1
        );
    }

    #[actix_web::test]
    async fn endpoint_distinguishes_pending_failed_stale_and_empty() {
        let state = AppState::for_test(None).await.unwrap();
        let app = actix_web::test::init_service(
            App::new()
                .app_data(state.clone())
                .service(super::super::handlers::list_external_parties),
        )
        .await;
        let request = || {
            actix_web::test::TestRequest::get()
                .uri("/external-parties")
                .to_request()
        };
        // Claim the first scan as a request would, so the handler sees it
        // running and does not reach for the Canton the test config lacks.
        assert!(state.external_parties.observe(Instant::now()).1);
        let response = actix_web::test::call_service(&app, request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = actix_web::test::read_body_json(response).await;
        assert_eq!(body["refreshing"], true);
        assert_eq!(body["fetched_at"], serde_json::Value::Null);
        assert_eq!(body["parties"], serde_json::json!([]));

        state
            .external_parties
            .finish(Err("Canton unavailable".into()), Instant::now());
        let response = actix_web::test::call_service(&app, request()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value = actix_web::test::read_body_json(response).await;
        assert_eq!(body["error"], "Canton unavailable");

        state
            .external_parties
            .finish(Ok(vec![party("wallet::key")]), Instant::now());
        state
            .external_parties
            .finish(Err("Canton hiccup".into()), Instant::now());
        let response = actix_web::test::call_service(&app, request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = actix_web::test::read_body_json(response).await;
        assert_eq!(body["parties"][0]["party_id"], "wallet::key");
        assert_eq!(body["refresh_error"], "Canton hiccup");
        assert_eq!(body["refreshing"], false);
        let fetched_at = body["fetched_at"].as_str().unwrap_or_default();
        assert!(
            DateTime::parse_from_rfc3339(fetched_at).is_ok(),
            "{fetched_at}"
        );

        state.external_parties.finish(Ok(vec![]), Instant::now());
        let response = actix_web::test::call_service(&app, request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = actix_web::test::read_body_json(response).await;
        assert_eq!(body["parties"], serde_json::json!([]));
        assert_eq!(body["refreshing"], false);
        assert_eq!(body["refresh_error"], serde_json::Value::Null);
        assert!(body["fetched_at"].is_string());
    }
}
