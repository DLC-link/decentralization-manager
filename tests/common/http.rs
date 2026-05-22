use anyhow::Context;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Serialize, de::DeserializeOwned};

use super::{Fixture, types::WorkflowRunsResponse};

impl Fixture {
    pub async fn post_json<B, R>(&self, port: u16, path: &str, body: &B) -> anyhow::Result<R>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let jwt = self.refresher.token().await.context("acquire bearer")?;
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {jwt}"))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .with_context(|| format!("read body POST {url}"))?;
        if !status.is_success() {
            anyhow::bail!(
                "POST {url} returned {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice::<R>(&bytes).with_context(|| {
            format!(
                "deserialize POST {url}: {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    }

    pub async fn get_json<R>(&self, port: u16, path: &str) -> anyhow::Result<R>
    where
        R: DeserializeOwned,
    {
        let jwt = self.refresher.token().await.context("acquire bearer")?;
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {jwt}"))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .with_context(|| format!("read body GET {url}"))?;
        if !status.is_success() {
            anyhow::bail!(
                "GET {url} returned {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice::<R>(&bytes)
            .with_context(|| format!("deserialize GET {url}: {}", String::from_utf8_lossy(&bytes)))
    }

    pub async fn put_json<B, R>(&self, port: u16, path: &str, body: &B) -> anyhow::Result<R>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let jwt = self.refresher.token().await.context("acquire bearer")?;
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .put(&url)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {jwt}"))
            .json(body)
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .with_context(|| format!("read body PUT {url}"))?;
        if !status.is_success() {
            anyhow::bail!(
                "PUT {url} returned {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice::<R>(&bytes)
            .with_context(|| format!("deserialize PUT {url}: {}", String::from_utf8_lossy(&bytes)))
    }

    /// POST that returns the HTTP status code (and body) without erroring on
    /// non-2xx. Used by tests that assert specific failure codes (409, 422,
    /// etc.) rather than the success-path JSON shape.
    pub async fn post_expect_status<B>(
        &self,
        port: u16,
        path: &str,
        body: &B,
    ) -> anyhow::Result<(reqwest::StatusCode, String)>
    where
        B: Serialize + ?Sized,
    {
        let jwt = self.refresher.token().await.context("acquire bearer")?;
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {jwt}"))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .with_context(|| format!("read body POST {url}"))?;
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// Probe `/workflows` until a coordinator-side run of `kind` reaches a
/// terminal state. The per-kind `/{kind}/status` endpoints were dropped in
/// favour of the generic `/workflows[/{instance_name}]` endpoints when
/// concurrent multi-instance workflows landed — `kind` here is the
/// `WorkflowKind` JSON enum value (e.g. "Onboarding", "Kick", "Contracts",
/// "Dars"). Picks the most recently-updated coordinator run of that kind so
/// stale terminal rows from earlier scenario phases don't pin the probe.
pub async fn probe_workflow_status(
    f: &Fixture,
    port: u16,
    kind: &str,
    label: &str,
) -> Option<anyhow::Result<()>> {
    let r: WorkflowRunsResponse = f.get_json(port, "/workflows").await.ok()?;
    // Pick the latest coordinator run of this kind. `/workflows` returns runs
    // ordered by `updated_at DESC` (newest first), so the first match in
    // iteration order is the freshly-started run for this phase rather than
    // a stale Completed row from an earlier phase.
    let run = r
        .runs
        .iter()
        .find(|w| w.kind == kind && w.role == "Coordinator")?;
    match run.status.as_str() {
        "completed" | "Completed" => Some(Ok(())),
        "failed" | "Failed" => Some(Err(anyhow::anyhow!(
            "{label} failed: {}",
            run.error.clone().unwrap_or_else(|| "unknown".into())
        ))),
        _ => None,
    }
}

/// Probe `GET /workflows` on `port` until a run matching `kind` + `role` +
/// `status` is visible. Used to assert the unified notification feed surfaces
/// completed/cancelled/failed runs from each side. Status values match the
/// JSON enum form returned by the handler (`completed`, `failed`, etc.).
pub async fn probe_workflow_run_visible(
    f: &Fixture,
    port: u16,
    kind: &str,
    role: &str,
    status: &str,
) -> Option<anyhow::Result<()>> {
    let r: WorkflowRunsResponse = f.get_json(port, "/workflows").await.ok()?;
    r.runs
        .iter()
        .any(|w| w.kind == kind && w.role == role && w.status == status)
        .then_some(Ok(()))
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::Fixture;

    async fn fixture_with_jwt(jwt: &str) -> (Fixture, MockServer) {
        let server = MockServer::start().await;
        let mut f = Fixture::for_test_with_jwt(jwt);
        f.p1.http = server.address().port();
        (f, server)
    }

    #[tokio::test]
    async fn get_json_attaches_bearer() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        Mock::given(method("GET"))
            .and(path("/ping"))
            .and(header("authorization", "Bearer test-jwt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let _: serde_json::Value = f.get_json(f.p1.http, "/ping").await.unwrap();
    }

    #[tokio::test]
    async fn post_json_attaches_bearer() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        Mock::given(method("POST"))
            .and(path("/ping"))
            .and(header("authorization", "Bearer test-jwt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let _: serde_json::Value = f
            .post_json(f.p1.http, "/ping", &serde_json::json!({}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn put_json_attaches_bearer() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        Mock::given(method("PUT"))
            .and(path("/ping"))
            .and(header("authorization", "Bearer test-jwt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let _: serde_json::Value = f
            .put_json(f.p1.http, "/ping", &serde_json::json!({}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn post_expect_status_attaches_bearer() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        Mock::given(method("POST"))
            .and(path("/ping"))
            .and(header("authorization", "Bearer test-jwt"))
            .respond_with(ResponseTemplate::new(422).set_body_string("nope"))
            .mount(&server)
            .await;

        let (status, _body) = f
            .post_expect_status(f.p1.http, "/ping", &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status.as_u16(), 422);
    }
}
