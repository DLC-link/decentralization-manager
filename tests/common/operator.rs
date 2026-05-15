use std::time::Duration;

use anyhow::Context;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::common::Fixture;

pub const OPERATOR_RESPONSE_TIMEOUT_DEVNET: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Error)]
#[error("operator automation did not produce {awaited} for action {action} within {elapsed:?}")]
pub struct OperatorAutomationTimeout {
    pub action: String,
    pub awaited: String,
    pub elapsed: Duration,
}

/// Poll `path` on `port`, deserialize the response as `R`, and call
/// `find_match` on each iteration. If `find_match` returns `Some(cid)`,
/// return it. If the timeout elapses, trip the fixture's circuit-breaker
/// and bail. If the breaker is already tripped, bail immediately without
/// polling.
pub async fn await_operator_response<R, F>(
    f: &mut Fixture,
    port: u16,
    path: &str,
    action: &str,
    awaited: &str,
    timeout: Duration,
    find_match: F,
) -> anyhow::Result<String>
where
    R: DeserializeOwned,
    F: Fn(R) -> Option<String>,
{
    if f.operator_timeout_tripped {
        anyhow::bail!(OperatorAutomationTimeout {
            action: action.to_string(),
            awaited: awaited.to_string(),
            elapsed: Duration::ZERO,
        });
    }

    let start = std::time::Instant::now();
    loop {
        let r: R = f
            .get_json(port, path)
            .await
            .with_context(|| format!("polling {path} for {awaited}"))?;
        if let Some(cid) = find_match(r) {
            return Ok(cid);
        }
        if start.elapsed() >= timeout {
            f.operator_timeout_tripped = true;
            anyhow::bail!(OperatorAutomationTimeout {
                action: action.to_string(),
                awaited: awaited.to_string(),
                elapsed: start.elapsed(),
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::common::Fixture;

    #[derive(Deserialize)]
    struct TestResp {
        contracts: Vec<TestContract>,
    }
    #[derive(Deserialize)]
    struct TestContract {
        contract_id: String,
    }

    async fn fixture_pointed_at(server: &MockServer) -> Fixture {
        let mut f = Fixture::for_test();
        f.p1.http = server.address().port();
        f
    }

    #[tokio::test]
    async fn returns_contract_when_match_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/q"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "contracts": [{"contract_id": "cid-42"}]
            })))
            .mount(&server)
            .await;

        let mut f = fixture_pointed_at(&server).await;
        let port = f.p1.http;
        let cid = await_operator_response::<TestResp, _>(
            &mut f,
            port,
            "/q",
            "TestAction",
            "TestContract",
            Duration::from_secs(2),
            |r| r.contracts.into_iter().next().map(|c| c.contract_id),
        )
        .await
        .unwrap();
        assert_eq!(cid, "cid-42");
        assert!(!f.operator_timeout_tripped);
    }

    #[tokio::test]
    async fn trips_circuit_breaker_on_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/q"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "contracts": []
            })))
            .mount(&server)
            .await;

        let mut f = fixture_pointed_at(&server).await;
        let port = f.p1.http;
        let err = await_operator_response::<TestResp, _>(
            &mut f,
            port,
            "/q",
            "TestAction",
            "TestContract",
            Duration::from_millis(50),
            |_| None,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("TestAction"));
        assert!(f.operator_timeout_tripped);
    }

    #[tokio::test]
    async fn fails_fast_when_circuit_breaker_tripped() {
        let server = MockServer::start().await;
        // No mock — verify the function returns BEFORE any HTTP call.
        let mut f = fixture_pointed_at(&server).await;
        f.operator_timeout_tripped = true;
        let port = f.p1.http;

        let err = await_operator_response::<TestResp, _>(
            &mut f,
            port,
            "/q",
            "TestAction",
            "TestContract",
            Duration::from_secs(60),
            |r| r.contracts.into_iter().next().map(|c| c.contract_id),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("TestAction"));

        // Verify the breaker short-circuited BEFORE any HTTP call — wiremock
        // received zero requests on the mounted MockServer.
        let received = server.received_requests().await.unwrap();
        assert_eq!(
            received.len(),
            0,
            "expected no HTTP requests but got {}",
            received.len()
        );
    }
}
