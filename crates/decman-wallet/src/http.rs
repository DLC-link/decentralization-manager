//! JSON over HTTP for the Ledger API and registry clients.
//!
//! Errors carry the base URL, the method and the path, like the tenant
//! client's, so a wallet talking to several services can tell them apart.

use std::time::Duration;

use reqwest::{Client, RequestBuilder, Response};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::error::{Error, Result};

/// How long one call may take. A prepare or an execute waits for Canton to
/// interpret or commit a transaction, so this matches the tenant client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// One base URL and the optional bearer token sent with every call to it.
#[derive(Clone)]
pub(crate) struct JsonHost {
    http: Client,
    base_url: String,
    token: Option<String>,
}

impl JsonHost {
    pub(crate) fn new(base_url: impl Into<String>, token: Option<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|source| Error::Client {
                host: base_url.clone(),
                source,
            })?;
        Ok(Self {
            http,
            base_url,
            token,
        })
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let request = self.http.get(format!("{base}{path}", base = self.base_url));
        let response = self.send("GET", path, request).await?;
        self.read_json("GET", path, response).await
    }

    pub(crate) async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let request = self
            .http
            .post(format!("{base}{path}", base = self.base_url))
            .json(body);
        let response = self.send("POST", path, request).await?;
        self.read_json("POST", path, response).await
    }

    /// POST to an endpoint that streams its answer. The body is a JSON array,
    /// newline-delimited JSON objects, or empty when there is nothing to send.
    pub(crate) async fn post_stream<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<Vec<T>> {
        let request = self
            .http
            .post(format!("{base}{path}", base = self.base_url))
            .json(body);
        let response = self.send("POST", path, request).await?;
        let response = self.check_status("POST", path, response).await?;
        let text = response.text().await.map_err(|source| Error::Decode {
            host: self.base_url.clone(),
            method: "POST",
            path: path.to_string(),
            source,
        })?;
        parse_stream(&text).map_err(|source| Error::Json {
            host: self.base_url.clone(),
            method: "POST",
            path: path.to_string(),
            source,
        })
    }

    async fn send(
        &self,
        method: &'static str,
        path: &str,
        request: RequestBuilder,
    ) -> Result<Response> {
        let request = match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        request.send().await.map_err(|source| Error::Transport {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            source,
        })
    }

    async fn read_json<T: DeserializeOwned>(
        &self,
        method: &'static str,
        path: &str,
        response: Response,
    ) -> Result<T> {
        let response = self.check_status(method, path, response).await?;
        response.json().await.map_err(|source| Error::Decode {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            source,
        })
    }

    /// Turn a non-2xx response into [`Error::Api`] with the server's message.
    async fn check_status(
        &self,
        method: &'static str,
        path: &str,
        response: Response,
    ) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        Err(Error::Api {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            status: status.as_u16(),
            message: error_message(body),
        })
    }
}

/// Read a streamed answer: a JSON array, one JSON object per line, or nothing.
fn parse_stream<T: DeserializeOwned>(text: &str) -> serde_json::Result<Vec<T>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.starts_with('[') {
        return serde_json::from_str(text);
    }
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// The readable part of an error body: a registry's `{"error": ...}`, or a
/// Canton error's code and cause. Anything else is returned as it came.
fn error_message(body: String) -> String {
    let Ok(value) = serde_json::from_str::<Value>(&body) else {
        return body;
    };
    if let Some(message) = value.get("error").and_then(Value::as_str) {
        return message.to_string();
    }
    let code = value.get("code").and_then(Value::as_str);
    match (code, value.get("cause").and_then(Value::as_str)) {
        (Some(code), Some(cause)) => format!("{code}: {cause}"),
        (None, Some(cause)) => cause.to_string(),
        _ => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_prefers_the_registry_error_then_the_canton_cause() {
        assert_eq!(
            error_message(r#"{"error": "No holdings provided"}"#.to_string()),
            "No holdings provided"
        );
        assert_eq!(
            error_message(r#"{"code": "NOT_FOUND", "cause": "no such contract"}"#.to_string()),
            "NOT_FOUND: no such contract"
        );
        assert_eq!(error_message("plain text".to_string()), "plain text");
    }
}
