use anyhow::Context;
use common::{
    api::WorkflowRunsResponse,
    types::{WorkflowKind, WorkflowProgress, WorkflowRole},
};
use reqwest::{
    StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    Fixture,
    probe::{Failure, snippet},
};

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

    async fn get_raw(
        &self,
        port: u16,
        path: &str,
    ) -> anyhow::Result<(String, StatusCode, Vec<u8>)> {
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
        Ok((url, status, bytes.to_vec()))
    }

    pub async fn get_json<R>(&self, port: u16, path: &str) -> anyhow::Result<R>
    where
        R: DeserializeOwned,
    {
        let (url, status, bytes) = self.get_raw(port, path).await?;
        if !status.is_success() {
            anyhow::bail!(
                "GET {url} returned {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice::<R>(&bytes)
            .with_context(|| format!("deserialize GET {url}: {}", String::from_utf8_lossy(&bytes)))
    }

    /// `get_json` for polled `Then` probes: `None` means "keep polling", and
    /// the reason is recorded in [`Fixture::probe_diag`] instead of being
    /// dropped by `.ok()?`. The runner logs that reason on timeout, and ends
    /// the step early once the same terminal failure — a mistyped path, an
    /// unparseable body — has repeated `FATAL_STREAK` times.
    pub async fn probe_get_json<R>(&self, port: u16, path: &str) -> Option<R>
    where
        R: DeserializeOwned,
    {
        let key = format!("GET :{port}{path}");
        let (url, status, bytes) = match self.get_raw(port, path).await {
            Ok(res) => res,
            Err(e) => {
                self.probe_diag
                    .record(&key, Failure::Transport.class(), format!("{key}: {e:#}"));
                return None;
            }
        };
        let body = String::from_utf8_lossy(&bytes);
        if !status.is_success() {
            let failure = Failure::Status(status, &body);
            self.probe_diag.record(
                &key,
                failure.class(),
                format!("GET {url} returned {status}: {}", snippet(&body)),
            );
            return None;
        }
        match serde_json::from_slice::<R>(&bytes) {
            Ok(v) => {
                self.probe_diag.record_ok(&key);
                Some(v)
            }
            Err(e) => {
                self.probe_diag.record(
                    &key,
                    Failure::Deserialize.class(),
                    format!("deserialize GET {url}: {e}: {}", snippet(&body)),
                );
                None
            }
        }
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

#[derive(Debug, Deserialize)]
struct WorkflowStatusResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub async fn probe_workflow_status(
    f: &Fixture,
    port: u16,
    path: &str,
    label: &str,
) -> Option<anyhow::Result<()>> {
    let s: WorkflowStatusResponse = f.probe_get_json(port, path).await?;
    match s.status.as_deref() {
        Some("completed") | Some("Completed") => Some(Ok(())),
        Some("failed") | Some("Failed") => Some(Err(anyhow::anyhow!(
            "{label} failed: {}",
            s.error.unwrap_or_else(|| "unknown".into())
        ))),
        _ => None,
    }
}

/// Probe `GET /workflows` on `port` until a run matching `kind` + `role` +
/// `status` is visible. Used to assert the unified notification feed surfaces
/// completed/cancelled/failed runs from each side.
pub async fn probe_workflow_run_visible(
    f: &Fixture,
    port: u16,
    kind: WorkflowKind,
    role: WorkflowRole,
    status: WorkflowProgress,
) -> Option<anyhow::Result<()>> {
    let r: WorkflowRunsResponse = f.probe_get_json(port, "/workflows").await?;
    r.runs
        .iter()
        .any(|w| w.kind == kind && w.role == role && w.status == status)
        .then_some(Ok(()))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::Fixture;
    use crate::common::probe::FATAL_STREAK;

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

    async fn mount_get(server: &MockServer, status: u16, body: &str) {
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(server)
            .await;
    }

    /// A mistyped probe path falls through to the frontend catch-all, so the
    /// body is not a JSON error object. That is the case #82 was filed for:
    /// it must be named, and must not burn the full deadline.
    #[tokio::test]
    async fn unrouted_404_is_fatal_after_the_streak() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        mount_get(&server, 404, "404 Not Found").await;

        for _ in 0..FATAL_STREAK {
            let v: Option<serde_json::Value> = f.probe_get_json(f.p1.http, "/probe").await;
            assert!(v.is_none());
        }

        let Some(fatal) = f.probe_diag.fatal() else {
            panic!("expected a recorded probe error");
        };
        assert!(fatal.contains("/probe"), "got: {fatal}");
        assert!(fatal.contains("404"), "got: {fatal}");
    }

    /// `GET /v0/tenant/{party}/status` answers 404 with a JSON error while
    /// onboarding is still in flight — polling it must stay a retry.
    #[tokio::test]
    async fn handler_issued_404_keeps_polling() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        mount_get(&server, 404, r#"{"error":"party not onboarded"}"#).await;

        for _ in 0..FATAL_STREAK * 2 {
            let v: Option<serde_json::Value> = f.probe_get_json(f.p1.http, "/probe").await;
            assert!(v.is_none());
        }

        assert!(f.probe_diag.fatal().is_none());
        let Some(last) = f.probe_diag.last_error() else {
            panic!("expected a recorded probe error");
        };
        assert!(last.contains("party not onboarded"), "got: {last}");
    }

    #[tokio::test]
    async fn server_errors_keep_polling_and_are_reported() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        mount_get(&server, 503, "upstream down").await;

        for _ in 0..FATAL_STREAK * 2 {
            let v: Option<serde_json::Value> = f.probe_get_json(f.p1.http, "/probe").await;
            assert!(v.is_none());
        }

        assert!(f.probe_diag.fatal().is_none());
        let Some(last) = f.probe_diag.last_error() else {
            panic!("expected a recorded probe error");
        };
        assert!(last.contains("503"), "got: {last}");
    }

    #[tokio::test]
    async fn success_body_the_probe_cannot_parse_is_fatal() {
        #[derive(Deserialize)]
        struct NeedsField {
            #[allow(dead_code)]
            required: String,
        }

        let (f, server) = fixture_with_jwt("test-jwt").await;
        mount_get(&server, 200, r#"{"other":1}"#).await;

        for _ in 0..FATAL_STREAK {
            let v: Option<NeedsField> = f.probe_get_json(f.p1.http, "/probe").await;
            assert!(v.is_none());
        }

        let Some(fatal) = f.probe_diag.fatal() else {
            panic!("expected a recorded probe error");
        };
        assert!(fatal.contains("deserialize"), "got: {fatal}");
        assert!(fatal.contains("required"), "got: {fatal}");
    }

    #[tokio::test]
    async fn a_successful_probe_leaves_no_diagnostics() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        mount_get(&server, 200, r#"{"ok":true}"#).await;

        let v: Option<serde_json::Value> = f.probe_get_json(f.p1.http, "/probe").await;
        assert!(v.is_some());
        assert!(f.probe_diag.last_error().is_none());
        assert!(f.probe_diag.fatal().is_none());
    }

    /// A connection to a port nothing is listening on is the restart-window
    /// case: retry, and never fail fast.
    #[tokio::test]
    async fn transport_failure_keeps_polling() {
        let (f, _server) = fixture_with_jwt("test-jwt").await;
        let Ok(reserved) = std::net::TcpListener::bind("127.0.0.1:0") else {
            panic!("could not reserve a loopback port");
        };
        let Ok(addr) = reserved.local_addr() else {
            panic!("reserved listener has no address");
        };
        let dead_port = addr.port();
        drop(reserved);

        for _ in 0..FATAL_STREAK * 2 {
            let v: Option<serde_json::Value> = f.probe_get_json(dead_port, "/probe").await;
            assert!(v.is_none());
        }

        let Some(last) = f.probe_diag.last_error() else {
            panic!("expected a recorded probe error");
        };
        assert!(f.probe_diag.fatal().is_none(), "got: {last}");
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
