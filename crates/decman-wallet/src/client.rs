//! A typed client for one DecMan host's tenant API (`/v0/tenant/*`).
//!
//! One `TenantClient` == one host. A co-validated party is hosted on several
//! participants, and the wallet talks to each of them directly — DecMan never
//! relays between hosts — so a wallet holds one client per host and drives them
//! itself (see [`crate::flow`]).

use base64::{Engine, engine::general_purpose::STANDARD};
use common::{
    api::{
        TenantAcsResponse, TenantContract, TenantExecuteSubmissionRequest, TenantOnboardRequest,
        TenantOnboardResponse, TenantPrepareRequest, TenantPrepareResponse,
        TenantPrepareSubmissionRequest, TenantPrepareSubmissionResponse,
    },
    types::WorkflowProgress,
};
use reqwest::{Client, StatusCode};
use serde::{Serialize, de::DeserializeOwned};

use crate::error::{Error, Result};

/// How long a single tenant-API call may take before the client gives up.
/// Onboarding calls do topology work on the node, so this is generous; the
/// wallet's own polling loop, not this timeout, decides how long to wait for a
/// party to come up.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// What one host reports about a party's onboarding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostStatus {
    /// This host's authorized `PartyToParticipant` names it — the party is live here.
    Hosted,
    /// Still a proposal on this host: it has not finished signing.
    Pending,
    /// This host has no mapping for the party at all.
    NotHosted,
}

impl HostStatus {
    /// Whether the party is fully live on this host.
    pub fn is_hosted(self) -> bool {
        matches!(self, Self::Hosted)
    }
}

/// A tenant-API client bound to a single DecMan host.
#[derive(Clone)]
pub struct TenantClient {
    http: Client,
    base_url: String,
    api_key: String,
}

impl TenantClient {
    /// Build a client for `base_url` (e.g. `https://node1.example.com`),
    /// authenticating with the provider-issued tenant API key.
    ///
    /// # Errors
    /// Returns [`Error::Client`] if the underlying HTTP client cannot be built.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
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
            api_key: api_key.into(),
        })
    }

    /// The host this client talks to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `POST /v0/tenant/prepare` — hand the node the party's public key and get
    /// back the unsigned multi-host topology plus the multi-hash to sign.
    pub async fn prepare(&self, req: &TenantPrepareRequest) -> Result<TenantPrepareResponse> {
        self.post("/v0/tenant/prepare", req).await
    }

    /// `POST /v0/tenant/onboard` — submit the wallet-signed bundle to THIS host.
    /// Idempotent, so a retry after a network blip is safe.
    pub async fn onboard(&self, req: &TenantOnboardRequest) -> Result<TenantOnboardResponse> {
        self.post("/v0/tenant/onboard", req).await
    }

    /// `GET /v0/tenant/{party}/status` — this host's view of the party.
    ///
    /// A 404 is not an error here: it is this host reporting that it does not
    /// host the party, which is a normal answer while onboarding is in flight.
    pub async fn host_status(&self, party_id: &str) -> Result<HostStatus> {
        let path = format!("/v0/tenant/{party_id}/status");
        match self.get::<common::api::WorkflowStatusResponse>(&path).await {
            Ok(resp) if resp.status == WorkflowProgress::Completed => Ok(HostStatus::Hosted),
            Ok(_) => Ok(HostStatus::Pending),
            Err(e) if e.is_status(StatusCode::NOT_FOUND.as_u16()) => Ok(HostStatus::NotHosted),
            Err(e) => Err(e),
        }
    }

    /// `POST /v0/tenant/{party}/prepare-submission` — build a CREATE command for
    /// the party and get back the prepared transaction plus the hash to sign.
    pub async fn prepare_submission(
        &self,
        party_id: &str,
        req: &TenantPrepareSubmissionRequest,
    ) -> Result<TenantPrepareSubmissionResponse> {
        self.post(&format!("/v0/tenant/{party_id}/prepare-submission"), req)
            .await
    }

    /// `POST /v0/tenant/{party}/execute-submission` — submit the wallet's
    /// signature over the prepared transaction. Returns an empty 200 on success.
    pub async fn execute_submission(
        &self,
        party_id: &str,
        req: &TenantExecuteSubmissionRequest,
    ) -> Result<()> {
        self.post_discarding_body(&format!("/v0/tenant/{party_id}/execute-submission"), req)
            .await
    }

    /// `GET /v0/tenant/{party}/acs` — the party's active contracts.
    pub async fn acs(&self, party_id: &str) -> Result<Vec<TenantContract>> {
        let resp: TenantAcsResponse = self.get(&format!("/v0/tenant/{party_id}/acs")).await?;
        Ok(resp.contracts)
    }

    /// Base64-decode a field the host sent us, tagging which field it was.
    pub(crate) fn decode_b64(&self, field: &'static str, value: &str) -> Result<Vec<u8>> {
        STANDARD.decode(value).map_err(|source| Error::Base64 {
            host: self.base_url.clone(),
            field,
            source,
        })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .http
            .get(format!("{base}{path}", base = self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|source| self.transport("GET", path, source))?;
        self.read_json("GET", path, response).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let response = self.send_post(path, body).await?;
        self.read_json("POST", path, response).await
    }

    /// For endpoints that answer with an empty 200 (`execute-submission`), where
    /// parsing a body would fail on success.
    async fn post_discarding_body<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let response = self.send_post(path, body).await?;
        self.check_status("POST", path, response).await.map(|_| ())
    }

    async fn send_post<B: Serialize>(&self, path: &str, body: &B) -> Result<reqwest::Response> {
        self.http
            .post(format!("{base}{path}", base = self.base_url))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|source| self.transport("POST", path, source))
    }

    async fn read_json<T: DeserializeOwned>(
        &self,
        method: &'static str,
        path: &str,
        response: reqwest::Response,
    ) -> Result<T> {
        let response = self.check_status(method, path, response).await?;
        response.json().await.map_err(|source| Error::Decode {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            source,
        })
    }

    /// Turn a non-2xx response into [`Error::Api`], preferring the server's
    /// `{"error": ...}` message over the raw body.
    async fn check_status(
        &self,
        method: &'static str,
        path: &str,
        response: reqwest::Response,
    ) -> Result<reqwest::Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or(body);
        Err(Error::Api {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            status: status.as_u16(),
            message,
        })
    }

    fn transport(&self, method: &'static str, path: &str, source: reqwest::Error) -> Error {
        Error::Transport {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            source,
        }
    }
}
