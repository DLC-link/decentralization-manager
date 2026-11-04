//! A client for one participant's JSON Ledger API (v2).
//!
//! The wallet reads its party's contracts and relays its signed submissions
//! through this client. The ledger user it authenticates as needs `CanReadAs`
//! and `CanExecuteAs` for the party, and never `CanActAs`: the participant
//! relays what the key holder signed and must not be able to submit on its own.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    error::Result,
    http::JsonHost,
    transfer::{HOLDING_INTERFACE_ID, TRANSFER_INSTRUCTION_INTERFACE_ID},
};

/// The `TransferInstructionStatus` constructor of an instruction that waits
/// for its receiver. The other one, `TransferPendingInternalWorkflow`, waits
/// for the registry.
const PENDING_RECEIVER_ACCEPTANCE: &str = "TransferPendingReceiverAcceptance";

/// A JSON Ledger API client for one participant.
#[derive(Clone)]
pub struct LedgerClient {
    host: JsonHost,
    user_id: String,
}

impl LedgerClient {
    /// Build a client for `base_url`, which includes any path prefix, for
    /// example `http://localhost:7575/api/json-api`. `token` is sent as a
    /// bearer token when present. `user_id` is the ledger user the
    /// submissions are made as.
    ///
    /// # Errors
    /// Returns [`Error::Client`](crate::Error::Client) if the HTTP client
    /// cannot be built.
    pub fn new(
        base_url: impl Into<String>,
        token: Option<String>,
        user_id: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            host: JsonHost::new(base_url, token)?,
            user_id: user_id.into(),
        })
    }

    /// The participant this client talks to.
    pub fn base_url(&self) -> &str {
        self.host.base_url()
    }

    /// The ledger user the submissions are made as.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// `GET /v2/state/ledger-end`: the offset an ACS read is taken at.
    ///
    /// # Errors
    /// Fails if the call fails or the answer has no offset.
    pub async fn ledger_end(&self) -> Result<i64> {
        let end: LedgerEnd = self.host.get("/v2/state/ledger-end").await?;
        Ok(end.offset)
    }

    /// The Token Standard holdings `party_id` owns, read through the
    /// `Holding` interface. Holdings the party can see but does not own are
    /// left out.
    ///
    /// # Errors
    /// Fails if either the ledger-end or the ACS call fails.
    pub async fn holdings(&self, party_id: &str) -> Result<Vec<Holding>> {
        let views: Vec<(String, HoldingView)> =
            self.interface_views(party_id, HOLDING_INTERFACE_ID).await?;
        Ok(views
            .into_iter()
            .filter(|(_, view)| view.owner == party_id)
            .map(|(contract_id, view)| Holding {
                contract_id,
                owner: view.owner,
                instrument_admin: view.instrument_id.admin,
                instrument_id: view.instrument_id.id,
                amount: view.amount,
                locked: view.lock.is_some(),
            })
            .collect())
    }

    /// The transfers `party_id` can accept now. Each names the party as its
    /// receiver, waits for the receiver's acceptance, and has not reached its
    /// `executeBefore`.
    ///
    /// An instruction that waits for the registry's internal workflow is left
    /// out, and so is an expired one. A deadline the client cannot read
    /// counts as expired.
    ///
    /// # Errors
    /// Fails if either the ledger-end or the ACS call fails.
    pub async fn transfer_instructions(&self, party_id: &str) -> Result<Vec<TransferInstruction>> {
        let views: Vec<(String, TransferInstructionView)> = self
            .interface_views(party_id, TRANSFER_INSTRUCTION_INTERFACE_ID)
            .await?;
        let now = Utc::now();
        Ok(views
            .into_iter()
            .filter(|(_, view)| {
                view.transfer.receiver == party_id
                    && view.status.tag == PENDING_RECEIVER_ACCEPTANCE
                    && DateTime::parse_from_rfc3339(&view.transfer.execute_before)
                        .is_ok_and(|deadline| deadline > now)
            })
            .map(|(contract_id, view)| TransferInstruction {
                contract_id,
                sender: view.transfer.sender,
                receiver: view.transfer.receiver,
                amount: view.transfer.amount,
                instrument_admin: view.transfer.instrument_id.admin,
                instrument_id: view.transfer.instrument_id.id,
                status: view.status.tag,
                execute_before: view.transfer.execute_before,
            })
            .collect())
    }

    /// `POST /v2/interactive-submission/prepare`.
    ///
    /// # Errors
    /// Fails if the call fails, for example when the command does not
    /// interpret.
    pub async fn prepare(&self, req: &PrepareRequest) -> Result<PreparedSubmission> {
        self.host
            .post("/v2/interactive-submission/prepare", req)
            .await
    }

    /// `POST /v2/interactive-submission/executeAndWait`.
    ///
    /// # Errors
    /// Fails if the call fails, for example when the signature is refused or
    /// the transaction does not commit.
    pub async fn execute(&self, req: &ExecuteRequest) -> Result<Executed> {
        self.host
            .post("/v2/interactive-submission/executeAndWait", req)
            .await
    }

    /// The active contracts `party_id` sees through `interface_id`, with
    /// their interface views. A contract whose view Canton could not compute
    /// is skipped.
    async fn interface_views<V: DeserializeOwned>(
        &self,
        party_id: &str,
        interface_id: &str,
    ) -> Result<Vec<(String, V)>> {
        let offset = self.ledger_end().await?;
        let request = json!({
            "activeAtOffset": offset,
            "eventFormat": {
                "filtersByParty": {
                    party_id: {
                        "cumulative": [{
                            "identifierFilter": {
                                "InterfaceFilter": {
                                    "value": {
                                        "interfaceId": interface_id,
                                        "includeInterfaceView": true,
                                        "includeCreatedEventBlob": false,
                                    }
                                }
                            }
                        }]
                    }
                },
                "verbose": false,
            },
        });
        let entries: Vec<AcsEntry<V>> = self
            .host
            .post_stream("/v2/state/active-contracts", &request)
            .await?;
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                let event = entry.contract_entry?.active_contract?.created_event;
                let view = event
                    .interface_views
                    .into_iter()
                    .find_map(|view| view.view_value)?;
                Some((event.contract_id, view))
            })
            .collect())
    }
}

/// A Token Standard holding, as its `Holding` interface view describes it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Holding {
    pub contract_id: String,
    pub owner: String,
    pub instrument_admin: String,
    pub instrument_id: String,
    /// The decimal amount as the ledger wrote it, for example `10.0000000000`.
    pub amount: String,
    /// Whether the holding carries a lock. A locked holding is held for a
    /// transfer or an allocation in flight and cannot be an input.
    pub locked: bool,
}

/// A transfer waiting for its receiver to accept it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TransferInstruction {
    pub contract_id: String,
    pub sender: String,
    pub receiver: String,
    /// The decimal amount as the ledger wrote it.
    pub amount: String,
    pub instrument_admin: String,
    pub instrument_id: String,
    /// The status constructor, `TransferPendingReceiverAcceptance` for every
    /// instruction [`LedgerClient::transfer_instructions`] returns.
    pub status: String,
    /// The deadline for accepting, RFC 3339, as the ledger wrote it.
    pub execute_before: String,
}

/// A contract the submission reads but the submitting party cannot see. The
/// registry and the Ledger API spell it the same way, so the registry's list
/// is passed on unchanged.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisclosedContract {
    pub contract_id: String,
    /// Base64, as the registry returned it.
    pub created_event_blob: String,
    pub synchronizer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
}

/// The body of `POST /v2/interactive-submission/prepare`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareRequest {
    pub user_id: String,
    pub command_id: String,
    pub act_as: Vec<String>,
    pub read_as: Vec<String>,
    /// JSON Ledger API commands, for example `{"ExerciseCommand": {...}}`.
    pub commands: Vec<Value>,
    pub disclosed_contracts: Vec<DisclosedContract>,
    /// `None` lets the participant choose the synchronizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synchronizer_id: Option<String>,
    pub verbose_hashing: bool,
}

/// What a prepare returns: the transaction, its hash, and the hashing scheme.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSubmission {
    /// The `PreparedTransaction` protobuf, base64.
    pub prepared_transaction: String,
    /// The hash to sign, base64.
    pub prepared_transaction_hash: String,
    /// For example `HASHING_SCHEME_VERSION_V2`.
    pub hashing_scheme_version: String,
}

/// The body of `POST /v2/interactive-submission/executeAndWait`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteRequest {
    pub user_id: String,
    pub submission_id: String,
    /// The prepared transaction, relayed unchanged.
    pub prepared_transaction: String,
    /// Echoed from the prepare.
    pub hashing_scheme_version: String,
    /// `{"Empty": {}}` uses the participant's default.
    pub deduplication_period: Value,
    /// `{"signatures": [{"party", "signatures": [...]}]}`.
    pub party_signatures: Value,
}

/// What an execute returns once the transaction has committed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Executed {
    pub update_id: String,
    pub completion_offset: i64,
}

#[derive(Deserialize)]
struct LedgerEnd {
    offset: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", bound = "V: DeserializeOwned")]
struct AcsEntry<V> {
    contract_entry: Option<ContractEntry<V>>,
}

/// One of `JsActiveContract`, `JsEmpty`, `JsIncompleteAssigned` or
/// `JsIncompleteUnassigned`. Only an active contract is a holding the party
/// has now.
#[derive(Deserialize)]
#[serde(bound = "V: DeserializeOwned")]
struct ContractEntry<V> {
    #[serde(rename = "JsActiveContract")]
    active_contract: Option<ActiveContract<V>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", bound = "V: DeserializeOwned")]
struct ActiveContract<V> {
    created_event: CreatedEvent<V>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", bound = "V: DeserializeOwned")]
struct CreatedEvent<V> {
    contract_id: String,
    #[serde(default = "Vec::new")]
    interface_views: Vec<InterfaceView<V>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", bound = "V: DeserializeOwned")]
struct InterfaceView<V> {
    view_value: Option<V>,
}

#[derive(Deserialize)]
struct InstrumentIdView {
    admin: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HoldingView {
    owner: String,
    instrument_id: InstrumentIdView,
    amount: String,
    lock: Option<Value>,
}

#[derive(Deserialize)]
struct TransferInstructionView {
    transfer: TransferView,
    status: StatusView,
}

/// A Daml variant in the JSON Ledger API: `{"tag": ..., "value": ...}`.
#[derive(Deserialize)]
struct StatusView {
    tag: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferView {
    sender: String,
    receiver: String,
    amount: String,
    instrument_id: InstrumentIdView,
    execute_before: String,
}
