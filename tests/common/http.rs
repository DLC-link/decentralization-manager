use anyhow::Context;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{Fixture, types::WorkflowRunsResponse};

impl Fixture {
    pub async fn post_json<B, R>(&self, port: u16, path: &str, body: &B) -> anyhow::Result<R>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", self.jwt))
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
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.jwt))
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
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .put(&url)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", self.jwt))
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
        let url = format!("http://localhost:{port}{path}");
        let res = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", self.jwt))
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
    let s: WorkflowStatusResponse = f.get_json(port, path).await.ok()?;
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
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::Fixture;

    async fn fixture_with_jwt(jwt: &str) -> (Fixture, MockServer) {
        let server = MockServer::start().await;
        let mut f = Fixture::for_test();
        f.jwt = jwt.to_string();
        f.p1.http = server.address().port();
        (f, server)
    }

    #[tokio::test]
    async fn get_json_attaches_bearer() {
        let (f, server) = fixture_with_jwt("test-jwt").await;
        Mock::given(method("GET"))
            .and(path("/ping"))
            .and(header("authorization", "Bearer test-jwt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})),
            )
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})),
            )
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})),
            )
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
