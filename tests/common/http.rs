use std::time::{Duration, Instant};

use anyhow::Context;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::Fixture;

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

pub async fn poll_until<T, F, Fut>(
    deadline: Duration,
    interval: Duration,
    label: &str,
    mut probe: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = probe().await {
            return Ok(v);
        }
        if start.elapsed() >= deadline {
            anyhow::bail!("polling timed out after {deadline:?}: {label}");
        }
        tokio::time::sleep(interval).await;
    }
}

pub async fn poll_workflow_status(
    f: &Fixture,
    port: u16,
    path: &str,
    label: &str,
) -> anyhow::Result<()> {
    let label_owned = label.to_string();
    let result: anyhow::Result<()> = poll_until(
        Duration::from_secs(240),
        Duration::from_secs(2),
        &format!("{label} workflow completion"),
        || {
            let label = label_owned.clone();
            async move {
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
        },
    )
    .await?;
    result
}

#[cfg(test)]
mod poll_tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    #[tokio::test]
    async fn poll_until_returns_immediately_on_success() {
        let v: u32 = poll_until(
            Duration::from_secs(1),
            Duration::from_millis(10),
            "always-some",
            || async { Some(42u32) },
        )
        .await
        .unwrap();
        assert_eq!(v, 42);
    }

    #[tokio::test]
    async fn poll_until_retries_until_success() {
        let counter = AtomicU32::new(0);
        let v: u32 = poll_until(
            Duration::from_secs(2),
            Duration::from_millis(10),
            "succeed-on-third",
            || async {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                (n >= 3).then_some(n)
            },
        )
        .await
        .unwrap();
        assert_eq!(v, 3);
    }

    #[tokio::test]
    async fn poll_until_times_out_with_label() {
        let started = Instant::now();
        let err = poll_until::<u32, _, _>(
            Duration::from_millis(80),
            Duration::from_millis(20),
            "never-ready",
            || async { None },
        )
        .await
        .unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(80));
        let msg = format!("{err}");
        assert!(msg.contains("never-ready") && msg.contains("timed out"));
    }
}
