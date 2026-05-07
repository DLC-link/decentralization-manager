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

    pub async fn post_json_auth<B, R>(&self, port: u16, path: &str, body: &B) -> anyhow::Result<R>
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
            .with_context(|| format!("POST(auth) {url}"))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .with_context(|| format!("read body POST(auth) {url}"))?;
        if !status.is_success() {
            anyhow::bail!(
                "POST(auth) {url} returned {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice::<R>(&bytes).with_context(|| {
            format!(
                "deserialize POST(auth) {url}: {}",
                String::from_utf8_lossy(&bytes)
            )
        })
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
