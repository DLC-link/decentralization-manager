//! A client for a Token Standard registry's off-ledger API.
//!
//! The registry is the instrument admin's service. It names the factory a
//! transfer runs through, and it resolves the choice context: the values a
//! Token Standard choice reads, and the contracts those values point at.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::Result,
    http::{ClientOptions, JsonHost},
    ledger::DisclosedContract,
    transfer::{TransferArgs, check_contract_id, transfer_choice_argument},
};

/// A registry client bound to one registry root.
#[derive(Clone)]
pub struct RegistryClient {
    host: JsonHost,
}

impl RegistryClient {
    /// Build a client for the registry at `root`. The client appends the
    /// `/registry/...` paths itself.
    ///
    /// For Canton Coin the root is a Splice scan's base URL. For a utility
    /// instrument such as CBTC it is
    /// `{utility}/api/token-standard/v0/registrars/{admin}`. `token` is sent
    /// as a bearer token when present. The client uses the default
    /// [`ClientOptions`].
    ///
    /// # Errors
    /// The errors of [`RegistryClient::with_options`].
    pub fn new(root: impl Into<String>, token: Option<String>) -> Result<Self> {
        Self::with_options(root, token, ClientOptions::default())
    }

    /// Like [`RegistryClient::new`], with a timeout, a body cap and a plain
    /// `http` rule of the caller's choosing.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`](crate::Error::InvalidUrl) for a URL that is not
    /// `http` or `https`. [`Error::InsecureUrl`](crate::Error::InsecureUrl)
    /// for plain `http` to a host that is not loopback, unless `options`
    /// allows it. [`Error::Client`](crate::Error::Client) if the HTTP client
    /// cannot be built.
    pub fn with_options(
        root: impl Into<String>,
        token: Option<String>,
        options: ClientOptions,
    ) -> Result<Self> {
        Ok(Self {
            host: JsonHost::with_options(root, token, options)?,
        })
    }

    /// The registry root this client talks to.
    pub fn root(&self) -> &str {
        self.host.base_url()
    }

    /// `POST {root}/registry/transfer-instruction/v1/transfer-factory`: the
    /// factory for this transfer and the context it needs.
    ///
    /// The registry resolves the context for these exact arguments, so the
    /// same `args` must go into the ledger command.
    ///
    /// # Errors
    /// Fails if the call fails. The registry explains a refusal in the
    /// message of [`Error::Api`](crate::Error::Api).
    pub async fn transfer_factory(&self, args: &TransferArgs) -> Result<TransferFactory> {
        let body = json!({
            "choiceArguments": transfer_choice_argument(args, &Value::Null),
            "excludeDebugFields": true,
        });
        self.host
            .post("/registry/transfer-instruction/v1/transfer-factory", &body)
            .await
    }

    /// `POST {root}/registry/transfer-instruction/v1/{cid}/choice-contexts/accept`:
    /// the context for accepting the transfer instruction `instruction_cid`.
    ///
    /// # Errors
    /// [`Error::InvalidContractId`](crate::Error::InvalidContractId) before
    /// any call when `instruction_cid` is not a contract id. Fails if the
    /// call fails.
    pub async fn accept_context(&self, instruction_cid: &str) -> Result<ChoiceContext> {
        check_contract_id(instruction_cid)?;
        let path =
            format!("/registry/transfer-instruction/v1/{instruction_cid}/choice-contexts/accept");
        self.host
            .post(&path, &json!({ "excludeDebugFields": true }))
            .await
    }
}

/// The registry's answer for a transfer.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferFactory {
    /// The contract the `TransferFactory_Transfer` choice is exercised on.
    pub factory_id: String,
    /// `offer` (the receiver must accept), `direct`, or `self`.
    pub transfer_kind: String,
    pub choice_context: ChoiceContext,
}

/// The context a Token Standard choice needs.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceContext {
    /// `{"values": {...}}`, passed into the choice as `extraArgs.context`.
    pub choice_context_data: Value,
    /// The contracts the context names. They go on the prepare.
    pub disclosed_contracts: Vec<DisclosedContract>,
}
