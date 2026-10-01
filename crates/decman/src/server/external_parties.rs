//! Topology discovery is expensive: Canton's participant filter scans the store.
//! A single background worker owns discovery; HTTP requests only read snapshots.
use std::time::Duration;

use actix_web::web;
use tokio::sync::RwLock;

use super::AppState;
use crate::workflow::external_party::steps::{HostedExternalParty, list_hosted_external_parties};

pub struct ExternalPartiesCache {
    snapshot: RwLock<Result<Vec<HostedExternalParty>, String>>,
}

impl Default for ExternalPartiesCache {
    fn default() -> Self {
        Self {
            snapshot: RwLock::new(Err(
                "External party discovery is in progress. Please try again shortly.".into(),
            )),
        }
    }
}

impl ExternalPartiesCache {
    pub async fn snapshot(&self) -> Result<Vec<HostedExternalParty>, String> {
        self.snapshot.read().await.clone()
    }

    async fn publish(&self, result: Result<Vec<HostedExternalParty>, String>) {
        *self.snapshot.write().await = result;
    }
}

pub(super) async fn refresh_forever(data: web::Data<AppState>) {
    loop {
        // Devnet scans can take several minutes. Bound a stuck scan, but allow
        // it to outlive HTTP/proxy deadlines. Never hold the cache lock over RPCs.
        let result = match tokio::time::timeout(
            Duration::from_secs(600),
            list_hosted_external_parties(&data.config),
        )
        .await
        {
            Ok(result) => result.map_err(|e| format!("Failed to discover external parties: {e:#}")),
            Err(_) => Err("External party discovery timed out. Retrying in the background.".into()),
        };
        if let Err(error) = &result {
            tracing::warn!(%error, "External party cache refresh failed");
        }
        // Publish failures too: never silently present an old snapshot as live.
        data.external_parties.publish(result).await;
        // Delay after completion, avoiding overlapping scans or catch-up bursts.
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, http::StatusCode, test};

    #[actix_web::test]
    async fn endpoint_serves_cache_and_distinguishes_unavailable_from_empty() {
        let state = AppState::for_test(None).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(state.clone())
                .service(super::super::handlers::list_external_parties),
        )
        .await;
        let request = || {
            test::TestRequest::get()
                .uri("/external-parties")
                .to_request()
        };
        assert_eq!(
            test::call_service(&app, request()).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        state
            .external_parties
            .publish(Ok(vec![HostedExternalParty {
                party_id: "wallet::key".into(),
                fingerprint: "key".into(),
                threshold: 1,
                host_count: 1,
                created_at: None,
                onboarding: false,
                hosts: vec![],
            }]))
            .await;
        // The default test config has no Canton: success proves the handler
        // reads the published snapshot without starting another topology scan.
        let response = test::call_service(&app, request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["parties"][0]["party_id"], "wallet::key");
        state
            .external_parties
            .publish(Err("Canton unavailable".into()))
            .await;
        assert_eq!(
            test::call_service(&app, request()).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        state.external_parties.publish(Ok(vec![])).await;
        let response = test::call_service(&app, request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["parties"], serde_json::json!([]));
    }
}
