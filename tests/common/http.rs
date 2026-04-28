use anyhow::Context;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Serialize, de::DeserializeOwned};

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
