//! Wire-contract tests for Token Standard transfers, against a stub JSON
//! Ledger API and a stub registry.
//!
//! One stub server plays both, under different path prefixes, so a test can
//! read the order of every call the wallet made.
//!
//! The prepared transactions the stub returns are real `PreparedTransaction`
//! protobuf messages, hashed by `decman_lib::canton_hash`. So the check the
//! wallet runs before it signs runs for real, against the same bytes a
//! participant would send.

use base64::{Engine, engine::general_purpose::STANDARD};
use decman_lib::{
    canton_hash::{HashingScheme, compute_prepared_transaction_hash},
    canton_proto_rs::com::daml::ledger::api::v2::{
        Identifier, List, Record, RecordField, TextMap, Value as DamlValue, Variant,
        interactive::{
            DamlTransaction, Metadata, PreparedTransaction,
            daml_transaction::{Node, NodeSeed, node::VersionedNode},
            metadata::SubmitterInfo,
            transaction::v1::{self, Exercise, node::NodeType},
        },
        text_map,
        value::Sum,
    },
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::Duration,
};

use decman_wallet::{
    AMULET_TRANSFER_INSTRUCTION_TEMPLATE, ClientOptions, Error, ExternalKeyPair, LedgerClient,
    Refusal, RegistryClient, TransferArgs, accept_transfer, send_transfer,
    transfer::{
        ACCEPT_CHOICE, HOLDING_INTERFACE_ID, TRANSFER_CHOICE, TRANSFER_FACTORY_INTERFACE_ID,
        TRANSFER_INSTRUCTION_INTERFACE_ID,
    },
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use prost::Message;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

const LEDGER_PREFIX: &str = "/api/json-api";
const REGISTRY_PREFIX: &str = "/scan";
const SENDER: &str = "alice::1220aa";
const RECEIVER: &str = "bob::1220bb";
const DSO: &str = "DSO::1220dd";

fn ledger_for(server: &MockServer) -> LedgerClient {
    match LedgerClient::new(
        format!("{uri}{LEDGER_PREFIX}", uri = server.uri()),
        Some("ledger-test-token".to_string()),
        "wallet-user",
    ) {
        Ok(client) => client,
        Err(e) => panic!("ledger client must build: {e}"),
    }
}

fn registry_for(server: &MockServer) -> RegistryClient {
    match RegistryClient::new(format!("{uri}{REGISTRY_PREFIX}", uri = server.uri()), None) {
        Ok(client) => client,
        Err(e) => panic!("registry client must build: {e}"),
    }
}

fn args() -> TransferArgs {
    TransferArgs {
        sender: SENDER.to_string(),
        receiver: RECEIVER.to_string(),
        amount: "1.25".to_string(),
        instrument_admin: DSO.to_string(),
        instrument_id: "Amulet".to_string(),
        input_holding_cids: Vec::new(),
        requested_at: "2026-11-04T10:00:00.000Z".to_string(),
        execute_before: "2026-11-05T10:00:00.000Z".to_string(),
        factory_template: None,
    }
}

/// One `JsGetActiveContractsResponse` carrying `view` as the contract's
/// interface view, shaped like Canton 3.6's JSON Ledger API.
fn acs_entry(contract_id: &str, interface: &str, view: Value) -> Value {
    json!({
        "workflowId": "",
        "contractEntry": {
            "JsActiveContract": {
                "createdEvent": {
                    "offset": 41,
                    "nodeId": 0,
                    "contractId": contract_id,
                    "templateId": "pkg:Splice.Amulet:Amulet",
                    "createArgument": {},
                    "interfaceViews": [{
                        "interfaceId": interface,
                        "viewStatus": { "code": 0, "message": "" },
                        "viewValue": view,
                    }],
                    "witnessParties": [SENDER],
                    "signatories": [DSO],
                    "createdAt": "2026-11-04T09:00:00Z",
                    "packageName": "splice-amulet",
                    "representativePackageId": "pkg",
                    "acsDelta": true,
                },
                "synchronizerId": "global-domain::1220ee",
                "reassignmentCounter": 0,
            }
        }
    })
}

fn holding_view(owner: &str, id: &str, amount: &str, lock: Value) -> Value {
    json!({
        "owner": owner,
        "instrumentId": { "admin": DSO, "id": id },
        "amount": amount,
        "lock": lock,
        "meta": { "values": {} },
    })
}

/// A deadline that has not passed, and one that has.
const OPEN_UNTIL: &str = "2999-01-01T00:00:00Z";
const EXPIRED_AT: &str = "2000-01-01T00:00:00Z";

/// The status of an instruction that waits for its receiver, as the JSON
/// Ledger API encodes a Daml variant.
fn awaiting_receiver() -> Value {
    json!({ "tag": "TransferPendingReceiverAcceptance", "value": {} })
}

fn instruction_view(
    sender: &str,
    receiver: &str,
    amount: &str,
    status: Value,
    execute_before: &str,
) -> Value {
    json!({
        "originalInstructionCid": null,
        "transfer": {
            "sender": sender,
            "receiver": receiver,
            "amount": amount,
            "instrumentId": { "admin": DSO, "id": "Amulet" },
            "requestedAt": "2000-01-01T00:00:00Z",
            "executeBefore": execute_before,
            "inputHoldingCids": [],
            "meta": { "values": {} },
        },
        "status": status,
        "meta": { "values": {} },
    })
}

async fn stub_ledger_end(server: &MockServer, offset: i64) {
    Mock::given(method("GET"))
        .and(path(format!("{LEDGER_PREFIX}/v2/state/ledger-end")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "offset": offset })))
        .mount(server)
        .await;
}

async fn stub_acs(server: &MockServer, entries: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path(format!("{LEDGER_PREFIX}/v2/state/active-contracts")))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Array(entries)))
        .mount(server)
        .await;
}

/// The JSON bodies the stub received on `request_path`, in order.
async fn bodies(server: &MockServer, request_path: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == request_path)
        .map(|r| match serde_json::from_slice(&r.body) {
            Ok(body) => body,
            Err(e) => panic!("{request_path} body must be JSON: {e}"),
        })
        .collect()
}

#[tokio::test]
async fn holdings_are_read_from_interface_views_and_locked_is_set_from_the_lock_field() {
    let server = MockServer::start().await;
    stub_ledger_end(&server, 42).await;
    let lock =
        json!({ "holders": [DSO], "expiresAt": null, "expiresAfter": null, "context": null });
    stub_acs(
        &server,
        vec![
            acs_entry(
                "00a1",
                "pkg:Splice.Api.Token.HoldingV1:Holding",
                holding_view(SENDER, "Amulet", "10.0000000000", Value::Null),
            ),
            acs_entry(
                "00a2",
                "pkg:Splice.Api.Token.HoldingV1:Holding",
                holding_view(SENDER, "Amulet", "2.5000000000", lock),
            ),
            // Visible to the sender, owned by someone else.
            acs_entry(
                "00a3",
                "pkg:Splice.Api.Token.HoldingV1:Holding",
                holding_view(RECEIVER, "Amulet", "7.0000000000", Value::Null),
            ),
            // Not an active contract: an empty entry carries no holding.
            json!({ "workflowId": "", "contractEntry": { "JsEmpty": {} } }),
        ],
    )
    .await;

    let holdings = match ledger_for(&server).holdings(SENDER).await {
        Ok(holdings) => holdings,
        Err(e) => panic!("holdings must read: {e}"),
    };

    let read: Vec<(&str, &str, bool)> = holdings
        .iter()
        .map(|h| (h.contract_id.as_str(), h.amount.as_str(), h.locked))
        .collect();
    assert_eq!(
        read,
        vec![
            ("00a1", "10.0000000000", false),
            ("00a2", "2.5000000000", true)
        ]
    );
    assert!(
        holdings
            .iter()
            .all(|h| h.owner == SENDER && h.instrument_admin == DSO)
    );

    let acs = bodies(
        &server,
        &format!("{LEDGER_PREFIX}/v2/state/active-contracts"),
    )
    .await;
    assert_eq!(acs.len(), 1);
    assert_eq!(
        acs[0]["activeAtOffset"], 42,
        "the ACS is read at the ledger end"
    );
    let filter = &acs[0]["eventFormat"]["filtersByParty"][SENDER]["cumulative"][0]["identifierFilter"]
        ["InterfaceFilter"]["value"];
    assert_eq!(filter["interfaceId"], HOLDING_INTERFACE_ID);
    assert_eq!(filter["includeInterfaceView"], true);

    // The ledger token goes with every call.
    for request in server.received_requests().await.unwrap_or_default() {
        let auth = request
            .headers
            .get("authorization")
            .map(|v| v.as_bytes().to_vec());
        assert_eq!(auth, Some(b"Bearer ledger-test-token".to_vec()));
    }
}

#[tokio::test]
async fn transfer_instructions_are_the_ones_naming_the_party_as_receiver() {
    let server = MockServer::start().await;
    stub_ledger_end(&server, 7).await;
    let interface = "pkg:Splice.Api.Token.TransferInstructionV1:TransferInstruction";
    let mut offer = acs_entry(
        "00b1",
        interface,
        instruction_view(
            SENDER,
            RECEIVER,
            "1.2500000000",
            awaiting_receiver(),
            OPEN_UNTIL,
        ),
    );
    offer["contractEntry"]["JsActiveContract"]["createdEvent"]["templateId"] =
        json!("amuletpkgid:Splice.AmuletTransferInstruction:AmuletTransferInstruction");
    stub_acs(
        &server,
        vec![
            offer,
            // The receiver's own outgoing offer: not one it can accept.
            acs_entry(
                "00b2",
                interface,
                instruction_view(
                    RECEIVER,
                    SENDER,
                    "3.0000000000",
                    awaiting_receiver(),
                    OPEN_UNTIL,
                ),
            ),
        ],
    )
    .await;

    let pending = match ledger_for(&server).transfer_instructions(RECEIVER).await {
        Ok(pending) => pending,
        Err(e) => panic!("transfer instructions must read: {e}"),
    };

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].contract_id, "00b1");
    assert_eq!(
        pending[0].template, AMULET_TRANSFER_INSTRUCTION_TEMPLATE,
        "the package id is replaced by the package name"
    );
    assert_eq!(pending[0].sender, SENDER);
    assert_eq!(pending[0].receiver, RECEIVER);
    assert_eq!(pending[0].amount, "1.2500000000");
    assert_eq!(pending[0].instrument_admin, DSO);
    assert_eq!(pending[0].instrument_id, "Amulet");
    assert_eq!(pending[0].status, "TransferPendingReceiverAcceptance");
    assert_eq!(pending[0].execute_before, OPEN_UNTIL);

    let acs = bodies(
        &server,
        &format!("{LEDGER_PREFIX}/v2/state/active-contracts"),
    )
    .await;
    let filter = &acs[0]["eventFormat"]["filtersByParty"][RECEIVER]["cumulative"][0]["identifierFilter"]
        ["InterfaceFilter"]["value"];
    assert_eq!(filter["interfaceId"], TRANSFER_INSTRUCTION_INTERFACE_ID);
}

/// Only an offer that waits for the receiver and has not expired can be
/// accepted, so only that one is listed.
#[tokio::test]
async fn transfer_instructions_are_only_the_offers_the_party_can_accept_now() {
    let server = MockServer::start().await;
    stub_ledger_end(&server, 7).await;
    let interface = "pkg:Splice.Api.Token.TransferInstructionV1:TransferInstruction";
    let internal_workflow = json!({
        "tag": "TransferPendingInternalWorkflow",
        "value": { "pendingActions": [[DSO, "approve the transfer"]] },
    });
    stub_acs(
        &server,
        vec![
            acs_entry(
                "00b1",
                interface,
                instruction_view(
                    SENDER,
                    RECEIVER,
                    "1.2500000000",
                    awaiting_receiver(),
                    OPEN_UNTIL,
                ),
            ),
            acs_entry(
                "00b2",
                interface,
                instruction_view(
                    SENDER,
                    RECEIVER,
                    "2.0000000000",
                    internal_workflow,
                    OPEN_UNTIL,
                ),
            ),
            acs_entry(
                "00b3",
                interface,
                instruction_view(
                    SENDER,
                    RECEIVER,
                    "3.0000000000",
                    awaiting_receiver(),
                    EXPIRED_AT,
                ),
            ),
        ],
    )
    .await;

    let pending = match ledger_for(&server).transfer_instructions(RECEIVER).await {
        Ok(pending) => pending,
        Err(e) => panic!("transfer instructions must read: {e}"),
    };

    let listed: Vec<&str> = pending.iter().map(|i| i.contract_id.as_str()).collect();
    assert_eq!(listed, vec!["00b1"]);
}

/// An operator reads the package id here to pin it in an accept.
#[tokio::test]
async fn transfer_instructions_carry_the_package_id_of_their_template() {
    let server = MockServer::start().await;
    stub_ledger_end(&server, 7).await;
    let mut offer = acs_entry(
        "00b1",
        "pkg:Splice.Api.Token.TransferInstructionV1:TransferInstruction",
        instruction_view(
            SENDER,
            RECEIVER,
            "1.2500000000",
            awaiting_receiver(),
            OPEN_UNTIL,
        ),
    );
    offer["contractEntry"]["JsActiveContract"]["createdEvent"]["templateId"] =
        json!("amuletpkgid:Splice.AmuletTransferInstruction:AmuletTransferInstruction");
    stub_acs(&server, vec![offer]).await;

    let pending = match ledger_for(&server).transfer_instructions(RECEIVER).await {
        Ok(pending) => pending,
        Err(e) => panic!("transfer instructions must read: {e}"),
    };

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].package_id, "amuletpkgid");
    assert_eq!(pending[0].template, AMULET_TRANSFER_INSTRUCTION_TEMPLATE);
}

/// The active-contracts endpoint answers with a JSON array, with one JSON
/// object per line, or with an empty body. Each must read the same.
#[tokio::test]
async fn holdings_read_every_body_shape_the_active_contracts_endpoint_sends() {
    let holding = "pkg:Splice.Api.Token.HoldingV1:Holding";
    let entries = [
        acs_entry(
            "00a1",
            holding,
            holding_view(SENDER, "Amulet", "10.0000000000", Value::Null),
        ),
        acs_entry(
            "00a2",
            holding,
            holding_view(SENDER, "Amulet", "2.5000000000", Value::Null),
        ),
    ];
    let array = Value::Array(entries.to_vec()).to_string();
    let ndjson = entries
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    for (shape, body, expected) in [
        ("a JSON array", array, vec!["00a1", "00a2"]),
        ("newline-delimited JSON", ndjson, vec!["00a1", "00a2"]),
        ("an empty body", String::new(), Vec::new()),
    ] {
        let server = MockServer::start().await;
        stub_ledger_end(&server, 42).await;
        Mock::given(method("POST"))
            .and(path(format!("{LEDGER_PREFIX}/v2/state/active-contracts")))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let holdings = match ledger_for(&server).holdings(SENDER).await {
            Ok(holdings) => holdings,
            Err(e) => panic!("{shape} must read: {e}"),
        };

        let read: Vec<&str> = holdings.iter().map(|h| h.contract_id.as_str()).collect();
        assert_eq!(read, expected, "{shape}");
    }
}

#[tokio::test]
async fn the_registry_is_asked_for_the_factory_with_the_transfer_arguments() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{REGISTRY_PREFIX}/registry/transfer-instruction/v1/transfer-factory")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "factoryId": "00f1",
            "transferKind": "offer",
            "choiceContext": {
                "choiceContextData": { "values": { "amulet-rules": { "tag": "AV_ContractId", "value": "00c1" } } },
                "disclosedContracts": [{
                    "templateId": "pkg:Splice.AmuletRules:AmuletRules",
                    "contractId": "00c1",
                    "createdEventBlob": "YmxvYg==",
                    "synchronizerId": "global-domain::1220ee",
                }],
            },
        })))
        .mount(&server)
        .await;

    let factory = match registry_for(&server).transfer_factory(&args()).await {
        Ok(factory) => factory,
        Err(e) => panic!("the factory must be read: {e}"),
    };

    assert_eq!(factory.factory_id, "00f1");
    assert_eq!(factory.transfer_kind, "offer");
    assert_eq!(factory.choice_context.disclosed_contracts.len(), 1);
    assert_eq!(
        factory.choice_context.disclosed_contracts[0].contract_id,
        "00c1"
    );

    let sent = bodies(
        &server,
        &format!("{REGISTRY_PREFIX}/registry/transfer-instruction/v1/transfer-factory"),
    )
    .await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["excludeDebugFields"], true);
    let choice = &sent[0]["choiceArguments"];
    assert_eq!(choice["expectedAdmin"], DSO);
    assert_eq!(
        choice["transfer"]["amount"], "1.25",
        "the amount is never reformatted"
    );
    assert_eq!(choice["transfer"]["meta"], json!({ "values": {} }));
    assert_eq!(choice["extraArgs"]["context"], json!({ "values": {} }));
    assert_eq!(
        choice["transfer"]["requestedAt"],
        "2026-11-04T10:00:00.000Z"
    );
}

#[tokio::test]
async fn a_registry_refusal_carries_its_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{REGISTRY_PREFIX}/registry/transfer-instruction/v1/transfer-factory"
        )))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({ "error": "No holdings provided" })),
        )
        .mount(&server)
        .await;

    let error = match registry_for(&server).transfer_factory(&args()).await {
        Ok(_) => panic!("a 400 from the registry must be an error"),
        Err(e) => e,
    };

    match &error {
        Error::Api {
            host,
            status,
            message,
            ..
        } => {
            assert_eq!(*status, 400);
            assert_eq!(message, "No holdings provided");
            assert!(
                host.ends_with(REGISTRY_PREFIX),
                "the registry must be named: {host}"
            );
        }
        other => panic!("expected an API error, got {other}"),
    }
}

#[tokio::test]
async fn a_ledger_api_refusal_carries_the_endpoint_and_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{LEDGER_PREFIX}/v2/state/ledger-end")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "code": "PERMISSION_DENIED",
            "cause": "the user may not read as alice",
            "context": {},
            "errorCategory": 7,
        })))
        .mount(&server)
        .await;

    let error = match ledger_for(&server).holdings(SENDER).await {
        Ok(_) => panic!("a 403 from the ledger must be an error"),
        Err(e) => e,
    };

    match &error {
        Error::Api {
            method,
            path,
            status,
            message,
            ..
        } => {
            assert_eq!(*method, "GET");
            assert_eq!(path, "/v2/state/ledger-end");
            assert_eq!(*status, 403);
            assert_eq!(message, "PERMISSION_DENIED: the user may not read as alice");
        }
        other => panic!("expected an API error, got {other}"),
    }
    assert!(error.host().ends_with(LEDGER_PREFIX));
}

// ---------------------------------------------------------------------------
// The send and accept flows, with the check before signing.
// ---------------------------------------------------------------------------

const FACTORY_ID: &str = "00f1";
/// A contract id in Canton's form: `00`, a 32-byte discriminator, and a
/// suffix.
const INSTRUCTION_CID: &str = "005a92f6f089f63f0d476ba2494aa0848b93dbd0bcb7f058544e10992e67f09c98ca111220a1b7c6f1aa933f882745c9a905739790";
const MALLORY: &str = "mallory::1220ff";
const PREPARE_PATH: &str = "/api/json-api/v2/interactive-submission/prepare";
const EXECUTE_PATH: &str = "/api/json-api/v2/interactive-submission/executeAndWait";

fn daml(sum: Sum) -> DamlValue {
    DamlValue { sum: Some(sum) }
}

fn record(fields: Vec<(&str, DamlValue)>) -> DamlValue {
    daml(Sum::Record(Record {
        record_id: None,
        fields: fields
            .into_iter()
            .map(|(label, value)| RecordField {
                label: label.to_string(),
                value: Some(value),
            })
            .collect(),
    }))
}

fn party(id: &str) -> DamlValue {
    daml(Sum::Party(id.to_string()))
}

fn text_map_of(entries: Vec<(&str, DamlValue)>) -> DamlValue {
    daml(Sum::TextMap(TextMap {
        entries: entries
            .into_iter()
            .map(|(key, value)| text_map::Entry {
                key: key.to_string(),
                value: Some(value),
            })
            .collect(),
    }))
}

fn any_contract_id(cid: &str) -> DamlValue {
    daml(Sum::Variant(Box::new(Variant {
        variant_id: None,
        constructor: "AV_ContractId".to_string(),
        value: Some(Box::new(daml(Sum::ContractId(cid.to_string())))),
    })))
}

fn empty_metadata() -> DamlValue {
    record(vec![("values", text_map_of(Vec::new()))])
}

/// `Metadata` with one entry the wallet never sends.
fn filled_metadata() -> DamlValue {
    record(vec![(
        "values",
        text_map_of(vec![("note", daml(Sum::Text("pay mallory".to_string())))]),
    )])
}

/// `ExtraArgs` with these context entries and an empty `meta`.
fn extra_args_with(context: Vec<(&str, DamlValue)>) -> DamlValue {
    record(vec![
        ("context", record(vec![("values", text_map_of(context))])),
        ("meta", empty_metadata()),
    ])
}

/// `ExtraArgs` as the ledger writes it for `context_data()`.
fn extra_args() -> DamlValue {
    extra_args_with(vec![("amulet-rules", any_contract_id("00c1"))])
}

/// `value` with the record field at `path` set to `new`, or removed when
/// `new` is `None`.
fn with_field(mut value: DamlValue, path: &str, new: Option<DamlValue>) -> DamlValue {
    let (parents, last) = path.rsplit_once('.').unwrap_or(("", path));
    let mut current = &mut value;
    for label in parents.split('.').filter(|label| !label.is_empty()) {
        current = match current.sum.as_mut() {
            Some(Sum::Record(record)) => record
                .fields
                .iter_mut()
                .find(|field| field.label == label)
                .and_then(|field| field.value.as_mut()),
            _ => None,
        }
        .unwrap_or_else(|| panic!("the fixture has no {label} on the way to {path}"));
    }
    let Some(Sum::Record(record)) = current.sum.as_mut() else {
        panic!("{path} is not a record field");
    };
    let position = record.fields.iter().position(|field| field.label == last);
    match (position, new) {
        (Some(index), Some(new)) => {
            if let Some(field) = record.fields.get_mut(index) {
                field.value = Some(new);
            }
        }
        (Some(index), None) => {
            record.fields.remove(index);
        }
        (None, Some(new)) => record.fields.push(RecordField {
            label: last.to_string(),
            value: Some(new),
        }),
        (None, None) => panic!("the fixture has no {path} to remove"),
    }
    value
}

/// `args()`'s `requestedAt` and `executeBefore`, in microseconds since the
/// epoch, as the ledger writes them.
const REQUESTED_AT_MICROS: i64 = 1_793_786_400_000_000;
const EXECUTE_BEFORE_MICROS: i64 = 1_793_872_800_000_000;

/// What the ledger puts in the transaction for a transfer. The amount is in
/// the ledger's own form, with ten decimals.
#[derive(Clone, Copy)]
struct OnLedger<'a> {
    expected_admin: &'a str,
    sender: &'a str,
    receiver: &'a str,
    amount: &'a str,
    instrument_id: &'a str,
    execute_before: i64,
    inputs: &'a [&'a str],
}

/// The transfer `send_args(sender)` asks for, as the ledger writes it. The
/// one input is the unlocked holding `stub_send` gives the sender.
fn faithful(sender: &str) -> OnLedger<'_> {
    OnLedger {
        expected_admin: DSO,
        sender,
        receiver: RECEIVER,
        amount: "1.2500000000",
        instrument_id: "Amulet",
        execute_before: EXECUTE_BEFORE_MICROS,
        inputs: &["00a1"],
    }
}

/// The `TransferFactory_Transfer` choice argument as a Daml value.
fn transfer_value(transfer: &OnLedger<'_>) -> DamlValue {
    record(vec![
        ("expectedAdmin", party(transfer.expected_admin)),
        (
            "transfer",
            record(vec![
                ("sender", party(transfer.sender)),
                ("receiver", party(transfer.receiver)),
                ("amount", daml(Sum::Numeric(transfer.amount.to_string()))),
                (
                    "instrumentId",
                    record(vec![
                        ("admin", party(DSO)),
                        ("id", daml(Sum::Text(transfer.instrument_id.to_string()))),
                    ]),
                ),
                ("requestedAt", daml(Sum::Timestamp(REQUESTED_AT_MICROS))),
                (
                    "executeBefore",
                    daml(Sum::Timestamp(transfer.execute_before)),
                ),
                (
                    "inputHoldingCids",
                    daml(Sum::List(List {
                        elements: transfer
                            .inputs
                            .iter()
                            .map(|cid| daml(Sum::ContractId((*cid).to_string())))
                            .collect(),
                    })),
                ),
                ("meta", empty_metadata()),
            ]),
        ),
        ("extraArgs", extra_args()),
    ])
}

/// The root exercise of a Token Standard transaction, through `interface`,
/// on Canton Coin's external party rules.
fn exercise(
    contract_id: &str,
    interface: &str,
    choice: &str,
    acting: &[&str],
    argument: DamlValue,
) -> Exercise {
    Exercise {
        lf_version: "2.1".to_string(),
        contract_id: contract_id.to_string(),
        package_name: "splice-amulet".to_string(),
        template_id: Some(Identifier {
            package_id: AMULET_PACKAGE_ID.to_string(),
            module_name: "Splice.ExternalPartyAmuletRules".to_string(),
            entity_name: "ExternalPartyAmuletRules".to_string(),
        }),
        signatories: vec![DSO.to_string()],
        stakeholders: vec![DSO.to_string()],
        acting_parties: acting.iter().map(|p| (*p).to_string()).collect(),
        interface_id: Some(Identifier {
            package_id: "transfer-instruction-package-id".to_string(),
            module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
            entity_name: interface.to_string(),
        }),
        choice_id: choice.to_string(),
        chosen_value: Some(argument),
        consuming: false,
        children: Vec::new(),
        exercise_result: None,
        choice_observers: Vec::new(),
        key: None,
        by_key: false,
        external_call_results: Vec::new(),
    }
}

/// A one-node transaction with `root` at its root, submitted as `act_as`.
fn prepared(root: Exercise, act_as: &str) -> PreparedTransaction {
    prepared_node(NodeType::Exercise(root), act_as)
}

/// A one-node transaction with a node of any kind at its root. Its command id
/// is `command-1` until [`for_command`] sets another.
fn prepared_node(root: NodeType, act_as: &str) -> PreparedTransaction {
    PreparedTransaction {
        transaction: Some(DamlTransaction {
            version: "2.1".to_string(),
            roots: vec!["0".to_string()],
            nodes: vec![Node {
                node_id: "0".to_string(),
                versioned_node: Some(VersionedNode::V1(v1::Node {
                    node_type: Some(root),
                })),
            }],
            node_seeds: vec![NodeSeed {
                node_id: 0,
                seed: vec![7; 32],
            }],
        }),
        metadata: Some(Metadata {
            submitter_info: Some(SubmitterInfo {
                act_as: vec![act_as.to_string()],
                command_id: "command-1".to_string(),
            }),
            synchronizer_id: "global-domain::1220ee".to_string(),
            mediator_group: 0,
            transaction_uuid: "4c6471d3-4e09-49dd-addf-6cd90e19c583".to_string(),
            preparation_time: 1_793_786_400_000_000,
            ..Metadata::default()
        }),
    }
}

/// `transaction` as prepared for the command `command_id`.
fn for_command(mut transaction: PreparedTransaction, command_id: &str) -> PreparedTransaction {
    if let Some(submitter) = transaction
        .metadata
        .as_mut()
        .and_then(|metadata| metadata.submitter_info.as_mut())
    {
        submitter.command_id = command_id.to_string();
    }
    transaction
}

fn hash_of(transaction: &PreparedTransaction) -> Vec<u8> {
    match compute_prepared_transaction_hash(HashingScheme::V2, transaction) {
        Ok(hash) => hash,
        Err(e) => panic!("the fixture must hash: {e}"),
    }
}

/// The prepare response for `transaction`, carrying `hash` as its hash.
fn prepare_response(transaction: &PreparedTransaction, hash: &[u8]) -> Value {
    json!({
        "preparedTransaction": STANDARD.encode(transaction.encode_to_vec()),
        "preparedTransactionHash": STANDARD.encode(hash),
        "hashingSchemeVersion": "HASHING_SCHEME_VERSION_V2",
    })
}

/// A faithful send transaction for `sender`.
fn faithful_send(sender: &str) -> PreparedTransaction {
    prepared(
        exercise(
            FACTORY_ID,
            "TransferFactory",
            TRANSFER_CHOICE,
            &[sender],
            transfer_value(&faithful(sender)),
        ),
        sender,
    )
}

/// What the stub participant answers a prepare with, given the command id
/// the wallet sent.
trait Preparer: Fn(&str) -> Value + Send + Sync + 'static {}

impl<F: Fn(&str) -> Value + Send + Sync + 'static> Preparer for F {}

/// A participant that prepares `transaction` for the command id the wallet
/// sends, and answers with what `respond` makes of it.
fn preparing(
    transaction: PreparedTransaction,
    respond: impl Fn(&PreparedTransaction) -> Value + Send + Sync + 'static,
) -> impl Preparer {
    move |command_id| respond(&for_command(transaction.clone(), command_id))
}

/// A participant that prepares `transaction` for the command id the wallet
/// sends, and answers with its V2 hash.
fn honestly(transaction: PreparedTransaction) -> impl Preparer {
    preparing(transaction, |transaction| {
        prepare_response(transaction, &hash_of(transaction))
    })
}

/// The command id the wallet sent with its prepare.
async fn sent_command_id(server: &MockServer) -> String {
    let prepares = bodies(server, PREPARE_PATH).await;
    match prepares
        .first()
        .and_then(|prepare| prepare["commandId"].as_str())
    {
        Some(command_id) if !command_id.is_empty() => command_id.to_string(),
        _ => panic!("the wallet must send a command id with its prepare"),
    }
}

fn context_data() -> Value {
    json!({ "values": { "amulet-rules": { "tag": "AV_ContractId", "value": "00c1" } } })
}

fn disclosed() -> Value {
    json!([{
        "templateId": "pkg:Splice.AmuletRules:AmuletRules",
        "contractId": "00c1",
        "createdEventBlob": "YmxvYg==",
        "synchronizerId": "global-domain::1220ee",
    }])
}

/// Canton Coin's factory template, pinned to `package_id`.
fn factory_pinned_to(package_id: &str) -> String {
    format!("{package_id}:Splice.ExternalPartyAmuletRules:ExternalPartyAmuletRules")
}

/// The contracts the registry discloses for a send: the context's contract,
/// then the factory, as Splice's registry names them.
fn factory_disclosed() -> Value {
    let mut contracts = disclosed();
    if let Some(list) = contracts.as_array_mut() {
        list.push(json!({
            "templateId": factory_pinned_to(AMULET_PACKAGE_ID),
            "contractId": FACTORY_ID,
            "createdEventBlob": "ZmFjdG9yeQ==",
            "synchronizerId": "global-domain::1220ee",
        }));
    }
    contracts
}

async fn stub_factory(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(format!(
            "{REGISTRY_PREFIX}/registry/transfer-instruction/v1/transfer-factory"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "factoryId": FACTORY_ID,
            "transferKind": "offer",
            "choiceContext": {
                "choiceContextData": context_data(),
                "disclosedContracts": factory_disclosed(),
            },
        })))
        .mount(server)
        .await;
}

async fn stub_prepare_and_execute(server: &MockServer, prepare: impl Preparer) {
    Mock::given(method("POST"))
        .and(path(PREPARE_PATH))
        .respond_with(move |request: &Request| {
            let command_id = serde_json::from_slice::<Value>(&request.body)
                .ok()
                .and_then(|body| body["commandId"].as_str().map(str::to_string))
                .unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(prepare(&command_id))
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(EXECUTE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "updateId": "update-1",
            "completionOffset": 43,
        })))
        .mount(server)
        .await;
}

/// A ledger and registry ready for `sender` to send: one unlocked Amulet
/// holding it owns, plus holdings the send must not spend.
async fn stub_send(server: &MockServer, sender: &str, prepare: impl Preparer) {
    let lock =
        json!({ "holders": [DSO], "expiresAt": null, "expiresAfter": null, "context": null });
    let holding = "pkg:Splice.Api.Token.HoldingV1:Holding";
    stub_ledger_end(server, 42).await;
    stub_acs(
        server,
        vec![
            acs_entry(
                "00a1",
                holding,
                holding_view(sender, "Amulet", "10.0000000000", Value::Null),
            ),
            acs_entry(
                "00a2",
                holding,
                holding_view(sender, "Amulet", "5.0000000000", lock),
            ),
            acs_entry(
                "00a3",
                holding,
                holding_view(RECEIVER, "Amulet", "7.0000000000", Value::Null),
            ),
            acs_entry(
                "00a4",
                holding,
                holding_view(sender, "CBTC", "1.0000000000", Value::Null),
            ),
        ],
    )
    .await;
    stub_factory(server).await;
    stub_prepare_and_execute(server, prepare).await;
}

fn send_args(sender: &str) -> TransferArgs {
    TransferArgs {
        sender: sender.to_string(),
        ..args()
    }
}

async fn count(server: &MockServer, request_path: &str) -> usize {
    bodies(server, request_path).await.len()
}

fn verifying_key(key: &ExternalKeyPair) -> VerifyingKey {
    match VerifyingKey::from_bytes(&key.public_key_bytes()) {
        Ok(verifying) => verifying,
        Err(e) => panic!("the published key must be a valid Ed25519 key: {e}"),
    }
}

/// The one signature in an execute body, and the party it signs for.
fn signature_of(execute: &Value) -> (String, String, Signature) {
    let entry = &execute["partySignatures"]["signatures"][0];
    let signature = &entry["signatures"][0];
    assert_eq!(signature["format"], "SIGNATURE_FORMAT_CONCAT");
    assert_eq!(
        signature["signingAlgorithmSpec"],
        "SIGNING_ALGORITHM_SPEC_ED25519"
    );
    let bytes: [u8; 64] = match signature["signature"]
        .as_str()
        .map(|encoded| STANDARD.decode(encoded).map(TryInto::try_into))
    {
        Some(Ok(Ok(bytes))) => bytes,
        _ => panic!("the signature must be 64 base64-encoded bytes"),
    };
    (
        entry["party"].as_str().unwrap_or_default().to_string(),
        signature["signedBy"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        Signature::from_bytes(&bytes),
    )
}

/// The seed must not appear in any request the wallet made.
async fn assert_no_seed_on_the_wire(server: &MockServer, key: &ExternalKeyPair) {
    let seed = key.seed_b64();
    for request in server.received_requests().await.unwrap_or_default() {
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            !body.contains(seed.as_str()),
            "the private seed must never appear in a request body"
        );
    }
}

#[tokio::test]
async fn send_transfer_runs_the_steps_in_order_and_signs_the_hash_locally() {
    let key = ExternalKeyPair::from_seed([21u8; 32]);
    let sender = key.party_id("alice");
    let server = MockServer::start().await;
    stub_send(&server, &sender, honestly(faithful_send(&sender))).await;

    let receipt = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(receipt) => receipt,
        Err(e) => panic!("the send must succeed: {e}"),
    };

    let transaction = for_command(faithful_send(&sender), &sent_command_id(&server).await);
    let hash = hash_of(&transaction);
    assert_eq!(receipt.update_id, "update-1");
    assert_eq!(receipt.completion_offset, 43);
    assert_eq!(receipt.factory_id, FACTORY_ID);
    assert_eq!(
        receipt.factory_template,
        Some(factory_pinned_to(AMULET_PACKAGE_ID)),
        "the receipt names the template of the factory's disclosed contract"
    );
    assert_eq!(receipt.transfer_kind, "offer");
    assert_eq!(receipt.input_holding_cids, vec!["00a1".to_string()]);
    assert_eq!(receipt.prepared_transaction_hash, STANDARD.encode(&hash));

    // Holdings, registry, prepare, execute: in that order, each once.
    let order: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(
        order,
        vec![
            format!("{LEDGER_PREFIX}/v2/state/ledger-end"),
            format!("{LEDGER_PREFIX}/v2/state/active-contracts"),
            format!("{REGISTRY_PREFIX}/registry/transfer-instruction/v1/transfer-factory"),
            PREPARE_PATH.to_string(),
            EXECUTE_PATH.to_string(),
        ]
    );

    // The registry is asked about the one unlocked Amulet holding the sender
    // owns, and the amount as given.
    let factory_request = &bodies(
        &server,
        &format!("{REGISTRY_PREFIX}/registry/transfer-instruction/v1/transfer-factory"),
    )
    .await[0]["choiceArguments"];
    assert_eq!(
        factory_request["transfer"]["inputHoldingCids"],
        json!(["00a1"])
    );
    assert_eq!(factory_request["transfer"]["amount"], "1.25");
    assert_eq!(factory_request["transfer"]["meta"], json!({ "values": {} }));

    // The prepare exercises the registry's factory with the registry's
    // context, acts as the sender alone, and carries the registry's
    // disclosed contracts.
    let prepare = &bodies(&server, PREPARE_PATH).await[0];
    assert_eq!(prepare["userId"], "wallet-user");
    assert_eq!(prepare["actAs"], json!([sender]));
    assert_eq!(prepare["readAs"], json!([]));
    // Canton 3.6 refuses a prepare body without these two fields.
    assert_eq!(prepare["synchronizerId"], "");
    assert_eq!(prepare["packageIdSelectionPreference"], json!([]));
    assert_eq!(prepare["disclosedContracts"], factory_disclosed());
    let command = &prepare["commands"][0]["ExerciseCommand"];
    assert_eq!(command["templateId"], TRANSFER_FACTORY_INTERFACE_ID);
    assert_eq!(command["contractId"], FACTORY_ID);
    assert_eq!(command["choice"], TRANSFER_CHOICE);
    let choice_argument = &command["choiceArgument"];
    assert_eq!(choice_argument["extraArgs"]["context"], context_data());
    assert_eq!(
        choice_argument["extraArgs"]["meta"],
        json!({ "values": {} })
    );
    // The registry resolved the context for these times, so the command
    // carries the same ones.
    for field in ["requestedAt", "executeBefore", "inputHoldingCids", "amount"] {
        assert_eq!(
            choice_argument["transfer"][field], factory_request["transfer"][field],
            "{field} must match what the registry was asked about"
        );
    }

    // The execute relays the prepared transaction unchanged, echoes the
    // scheme, and carries one signature that verifies over the hash under
    // the key the wallet publishes.
    let execute = &bodies(&server, EXECUTE_PATH).await[0];
    assert_eq!(
        execute["preparedTransaction"],
        STANDARD.encode(transaction.encode_to_vec())
    );
    assert_eq!(execute["hashingSchemeVersion"], "HASHING_SCHEME_VERSION_V2");
    assert_eq!(execute["deduplicationPeriod"], json!({ "Empty": {} }));
    assert_eq!(execute["userId"], "wallet-user");
    let (signed_party, signed_by, signature) = signature_of(execute);
    assert_eq!(signed_party, sender);
    assert_eq!(signed_by, key.fingerprint());
    let verifying = verifying_key(&key);
    assert!(
        verifying.verify(&hash, &signature).is_ok(),
        "the signature must verify over the recomputed hash"
    );
    assert!(verifying.verify(b"another hash", &signature).is_err());

    assert_no_seed_on_the_wire(&server, &key).await;
}

#[tokio::test]
async fn send_transfer_refuses_when_the_party_holds_nothing_unlocked() {
    let key = ExternalKeyPair::from_seed([22u8; 32]);
    let sender = key.party_id("alice");
    let lock =
        json!({ "holders": [DSO], "expiresAt": null, "expiresAfter": null, "context": null });
    let holding = "pkg:Splice.Api.Token.HoldingV1:Holding";
    let server = MockServer::start().await;
    stub_ledger_end(&server, 42).await;
    stub_acs(
        &server,
        vec![
            acs_entry(
                "00a2",
                holding,
                holding_view(&sender, "Amulet", "5.0000000000", lock),
            ),
            acs_entry(
                "00a3",
                holding,
                holding_view(RECEIVER, "Amulet", "7.0000000000", Value::Null),
            ),
        ],
    )
    .await;
    stub_factory(&server).await;
    stub_prepare_and_execute(&server, honestly(faithful_send(&sender))).await;

    let error = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(_) => panic!("a send with nothing to spend must fail"),
        Err(e) => e,
    };

    assert!(
        matches!(&error, Error::NoHoldings { party, instrument_id, .. }
            if *party == sender && instrument_id == "Amulet"),
        "unexpected error: {error}"
    );
    let factory_path =
        format!("{REGISTRY_PREFIX}/registry/transfer-instruction/v1/transfer-factory");
    assert_eq!(
        count(&server, &factory_path).await,
        0,
        "the registry is not asked"
    );
    assert_eq!(count(&server, PREPARE_PATH).await, 0, "nothing is prepared");
}

/// The attack the recomputation stops: the participant shows one transaction
/// and asks for a signature over the hash of another.
#[tokio::test]
async fn a_hash_that_does_not_match_the_transaction_is_not_signed() {
    let key = ExternalKeyPair::from_seed([23u8; 32]);
    let sender = key.party_id("alice");
    let shown = faithful_send(&sender);
    let other = prepared(
        exercise(
            FACTORY_ID,
            "TransferFactory",
            TRANSFER_CHOICE,
            &[&sender],
            transfer_value(&OnLedger {
                receiver: MALLORY,
                ..faithful(&sender)
            }),
        ),
        &sender,
    );
    let server = MockServer::start().await;
    stub_send(
        &server,
        &sender,
        preparing(shown, move |shown| {
            prepare_response(shown, &hash_of(&other))
        }),
    )
    .await;

    let error = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(_) => panic!("a hash that is not the transaction's must not be signed"),
        Err(e) => e,
    };

    assert!(
        matches!(
            &error,
            Error::TransactionRefused {
                reason: Refusal::HashMismatch { .. },
                ..
            }
        ),
        "unexpected error: {error}"
    );
    assert!(
        error.host().ends_with(LEDGER_PREFIX),
        "the preparing participant is named"
    );
    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
}

/// The attack the intent check stops: a correctly hashed transaction that
/// does something other than what the wallet asked for.
#[tokio::test]
async fn a_transaction_for_a_different_receiver_is_not_signed() {
    let key = ExternalKeyPair::from_seed([24u8; 32]);
    let sender = key.party_id("alice");
    let redirected = prepared(
        exercise(
            FACTORY_ID,
            "TransferFactory",
            TRANSFER_CHOICE,
            &[&sender],
            transfer_value(&OnLedger {
                receiver: MALLORY,
                ..faithful(&sender)
            }),
        ),
        &sender,
    );
    let server = MockServer::start().await;
    stub_send(&server, &sender, honestly(redirected)).await;

    let error = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(_) => panic!("a transfer to someone else must not be signed"),
        Err(e) => e,
    };

    match &error {
        Error::TransactionRefused {
            reason:
                Refusal::Mismatch {
                    field,
                    expected,
                    found,
                },
            ..
        } => {
            assert_eq!(*field, "transfer.receiver");
            assert_eq!(expected, RECEIVER);
            assert_eq!(found, MALLORY);
        }
        other => panic!("expected a receiver mismatch, got {other}"),
    }
    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
}

/// Every field the check reads is compared: one tampered field is enough to
/// refuse.
#[tokio::test]
async fn each_field_the_check_reads_refuses_a_tampered_transaction() {
    let key = ExternalKeyPair::from_seed([25u8; 32]);
    let sender = key.party_id("alice");
    let honest = faithful(&sender);
    let tampered = [
        (
            "transfer.amount",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&OnLedger {
                    amount: "12.5000000000",
                    ..honest
                }),
            ),
        ),
        (
            "transfer.instrumentId.id",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&OnLedger {
                    instrument_id: "CBTC",
                    ..honest
                }),
            ),
        ),
        (
            "transfer.sender",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&OnLedger {
                    sender: MALLORY,
                    ..honest
                }),
            ),
        ),
        (
            "expectedAdmin",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&OnLedger {
                    expected_admin: MALLORY,
                    ..honest
                }),
            ),
        ),
        (
            // Keeps the offer open a day longer than the wallet asked.
            "transfer.executeBefore",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&OnLedger {
                    execute_before: EXECUTE_BEFORE_MICROS + 86_400_000_000,
                    ..honest
                }),
            ),
        ),
        (
            // Spends a holding the wallet did not choose.
            "transfer.inputHoldingCids",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&OnLedger {
                    inputs: &["00a1", "00a2"],
                    ..honest
                }),
            ),
        ),
        (
            "acting parties",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender, MALLORY],
                transfer_value(&honest),
            ),
        ),
        (
            "contract id",
            exercise(
                "00f2",
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&honest),
            ),
        ),
        (
            "choice",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                "TransferFactory_PublicFetch",
                &[&sender],
                transfer_value(&honest),
            ),
        ),
        (
            "interface",
            exercise(
                FACTORY_ID,
                "AllocationFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&honest),
            ),
        ),
    ];

    for (expected_field, root) in tampered {
        let server = MockServer::start().await;
        stub_send(&server, &sender, honestly(prepared(root, &sender))).await;

        let result = send_transfer(
            &ledger_for(&server),
            &registry_for(&server),
            &key,
            &send_args(&sender),
        )
        .await;

        match result {
            Err(Error::TransactionRefused {
                reason: Refusal::Mismatch { field, .. },
                ..
            }) => assert_eq!(field, expected_field),
            Err(other) => panic!("{expected_field}: expected a mismatch, got {other}"),
            Ok(_) => panic!("{expected_field}: a tampered transaction must not be signed"),
        }
        assert_eq!(
            count(&server, EXECUTE_PATH).await,
            0,
            "{expected_field}: nothing is executed"
        );
    }
}

/// The participant must not buy a weaker check by naming a scheme the
/// wallet cannot reproduce.
#[tokio::test]
async fn an_unsupported_hashing_scheme_is_not_signed() {
    let key = ExternalKeyPair::from_seed([26u8; 32]);
    let sender = key.party_id("alice");
    let named_v4 = preparing(faithful_send(&sender), |transaction| {
        let mut response = prepare_response(transaction, &hash_of(transaction));
        response["hashingSchemeVersion"] = json!("HASHING_SCHEME_VERSION_V4");
        response
    });
    let server = MockServer::start().await;
    stub_send(&server, &sender, named_v4).await;

    let error = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(_) => panic!("a scheme the wallet cannot reproduce must not be signed"),
        Err(e) => e,
    };

    assert!(
        matches!(
            &error,
            Error::TransactionRefused {
                reason: Refusal::Unverifiable(_),
                ..
            }
        ),
        "unexpected error: {error}"
    );
    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
}

fn accept_path() -> String {
    format!(
        "{REGISTRY_PREFIX}/registry/transfer-instruction/v1/{INSTRUCTION_CID}/choice-contexts/accept"
    )
}

/// A registry ready to give the context for accepting `INSTRUCTION_CID`.
async fn stub_accept_context(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(accept_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choiceContextData": context_data(),
            "disclosedContracts": disclosed(),
        })))
        .mount(server)
        .await;
}

/// A ledger and registry ready for `receiver` to accept `INSTRUCTION_CID`.
async fn stub_accept(server: &MockServer, prepare: impl Preparer) {
    stub_accept_context(server).await;
    stub_prepare_and_execute(server, prepare).await;
}

/// `root` acting on `template`, given as `#package-name:Module:Entity`.
fn on_template(root: Exercise, template: &str) -> Exercise {
    let parts: Vec<&str> = template.trim_start_matches('#').splitn(3, ':').collect();
    let [package_name, module_name, entity_name] = parts.as_slice() else {
        panic!("{template} must be #package-name:Module:Entity");
    };
    Exercise {
        package_name: (*package_name).to_string(),
        template_id: Some(Identifier {
            package_id: format!("{package_name}-package-id"),
            module_name: (*module_name).to_string(),
            entity_name: (*entity_name).to_string(),
        }),
        ..root
    }
}

/// The root of an accept of a Canton Coin instruction, with `extra_args` as
/// its argument's `extraArgs`.
fn accept_exercise(contract_id: &str, receiver: &str, extra_args: DamlValue) -> Exercise {
    on_template(
        exercise(
            contract_id,
            "TransferInstruction",
            ACCEPT_CHOICE,
            &[receiver],
            record(vec![("extraArgs", extra_args)]),
        ),
        AMULET_TRANSFER_INSTRUCTION_TEMPLATE,
    )
}

fn accept_transaction(contract_id: &str, receiver: &str) -> PreparedTransaction {
    prepared(
        accept_exercise(contract_id, receiver, extra_args()),
        receiver,
    )
}

/// Run an accept of `INSTRUCTION_CID` against a participant that prepares
/// `root`, and return the refusal. Panics unless the wallet refused to sign
/// and executed nothing.
async fn refused_accept(seed: u8, root: impl FnOnce(&str) -> Exercise) -> Refusal {
    refused_accept_expecting(seed, AMULET_TRANSFER_INSTRUCTION_TEMPLATE, root).await
}

/// Like [`refused_accept`], with the accept expecting `template`.
async fn refused_accept_expecting(
    seed: u8,
    template: &str,
    root: impl FnOnce(&str) -> Exercise,
) -> Refusal {
    let key = ExternalKeyPair::from_seed([seed; 32]);
    let receiver = key.party_id("bob");
    let server = MockServer::start().await;
    stub_accept(&server, honestly(prepared(root(&receiver), &receiver))).await;

    let result = accept_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &receiver,
        INSTRUCTION_CID,
        template,
    )
    .await;

    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
    match result {
        Err(Error::TransactionRefused { reason, .. }) => reason,
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("the accept must not be signed"),
    }
}

/// Run a send against a participant that prepares `root` for the sender,
/// and return the refusal. Panics unless the wallet refused to sign and
/// executed nothing.
async fn refused_send(seed: u8, root: impl FnOnce(&str) -> Exercise) -> Refusal {
    refused_send_of(seed, |sender| prepared(root(sender), sender)).await
}

/// Like [`refused_send`], for a whole transaction.
async fn refused_send_of(
    seed: u8,
    transaction: impl FnOnce(&str) -> PreparedTransaction,
) -> Refusal {
    refused_send_against(seed, None, |sender| {
        // A transaction the hasher cannot hash gets a stand-in hash. The
        // wallet must refuse it when it fails to recompute the hash.
        preparing(transaction(sender), |transaction| {
            let hash = compute_prepared_transaction_hash(HashingScheme::V2, transaction)
                .unwrap_or_else(|_| vec![0; 32]);
            prepare_response(transaction, &hash)
        })
    })
    .await
}

/// Run a send that expects `factory_template` against the participant
/// `participant` makes for the sender, and return the refusal. Panics
/// unless the wallet refused to sign and executed nothing.
async fn refused_send_against<P: Preparer>(
    seed: u8,
    factory_template: Option<&str>,
    participant: impl FnOnce(&str) -> P,
) -> Refusal {
    let key = ExternalKeyPair::from_seed([seed; 32]);
    let sender = key.party_id("alice");
    let server = MockServer::start().await;
    stub_send(&server, &sender, participant(&sender)).await;
    let args = TransferArgs {
        factory_template: factory_template.map(str::to_string),
        ..send_args(&sender)
    };

    let result = send_transfer(&ledger_for(&server), &registry_for(&server), &key, &args).await;

    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
    match result {
        Err(Error::TransactionRefused { reason, .. }) => reason,
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("the send must not be signed"),
    }
}

#[tokio::test]
async fn accept_transfer_exercises_the_instruction_with_the_registry_context() {
    let key = ExternalKeyPair::from_seed([27u8; 32]);
    let receiver = key.party_id("bob");
    let server = MockServer::start().await;
    stub_accept(
        &server,
        honestly(accept_transaction(INSTRUCTION_CID, &receiver)),
    )
    .await;

    let receipt = match accept_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &receiver,
        INSTRUCTION_CID,
        AMULET_TRANSFER_INSTRUCTION_TEMPLATE,
    )
    .await
    {
        Ok(receipt) => receipt,
        Err(e) => panic!("the accept must succeed: {e}"),
    };

    let transaction = for_command(
        accept_transaction(INSTRUCTION_CID, &receiver),
        &sent_command_id(&server).await,
    );
    let hash = hash_of(&transaction);
    assert_eq!(receipt.update_id, "update-1");
    assert_eq!(receipt.instruction_cid, INSTRUCTION_CID);
    assert_eq!(receipt.prepared_transaction_hash, STANDARD.encode(&hash));

    let order: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(
        order,
        vec![
            accept_path(),
            PREPARE_PATH.to_string(),
            EXECUTE_PATH.to_string()
        ]
    );
    assert_eq!(
        bodies(&server, &accept_path()).await[0],
        json!({ "excludeDebugFields": true }),
        "the same body the SDK sends"
    );

    let prepare = &bodies(&server, PREPARE_PATH).await[0];
    assert_eq!(prepare["actAs"], json!([receiver]));
    assert_eq!(prepare["synchronizerId"], "");
    assert_eq!(prepare["packageIdSelectionPreference"], json!([]));
    assert_eq!(prepare["disclosedContracts"], disclosed());
    let command = &prepare["commands"][0]["ExerciseCommand"];
    assert_eq!(command["templateId"], TRANSFER_INSTRUCTION_INTERFACE_ID);
    assert_eq!(command["contractId"], INSTRUCTION_CID);
    assert_eq!(command["choice"], ACCEPT_CHOICE);
    assert_eq!(
        command["choiceArgument"],
        json!({ "extraArgs": { "context": context_data(), "meta": { "values": {} } } })
    );

    let execute = &bodies(&server, EXECUTE_PATH).await[0];
    assert_eq!(
        execute["preparedTransaction"],
        STANDARD.encode(transaction.encode_to_vec())
    );
    assert_eq!(execute["hashingSchemeVersion"], "HASHING_SCHEME_VERSION_V2");
    let (signed_party, signed_by, signature) = signature_of(execute);
    assert_eq!(signed_party, receiver);
    assert_eq!(signed_by, key.fingerprint());
    assert!(verifying_key(&key).verify(&hash, &signature).is_ok());

    assert_no_seed_on_the_wire(&server, &key).await;
}

#[tokio::test]
async fn an_accept_prepared_for_another_instruction_is_not_signed() {
    let key = ExternalKeyPair::from_seed([28u8; 32]);
    let receiver = key.party_id("bob");
    let other = "00b94be5545fed18b849ded12c42afc6874093183fc8b14afc1c43a3c6dea15d8cca11122066570dd9d0a5e9c559d3d8b6830f3de9";
    let server = MockServer::start().await;
    stub_accept(&server, honestly(accept_transaction(other, &receiver))).await;

    let result = accept_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &receiver,
        INSTRUCTION_CID,
        AMULET_TRANSFER_INSTRUCTION_TEMPLATE,
    )
    .await;

    match result {
        Err(Error::TransactionRefused {
            reason: Refusal::Mismatch { field, found, .. },
            ..
        }) => {
            assert_eq!(field, "contract id");
            assert_eq!(found, other);
        }
        Err(other) => panic!("expected a contract id mismatch, got {other}"),
        Ok(_) => panic!("an accept of another instruction must not be signed"),
    }
    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
}

/// Any template can implement `TransferInstruction`. An accept prepared on a
/// template the caller did not name is refused, even with the right
/// interface, choice, contract id and party.
#[tokio::test]
async fn an_accept_prepared_on_another_template_is_not_signed() {
    for (seed, template) in [
        (
            29,
            "#mallory-amulet:Splice.AmuletTransferInstruction:AmuletTransferInstruction",
        ),
        (
            30,
            "#splice-amulet:Splice.AmuletTransferInstruction:DrainOnAccept",
        ),
    ] {
        let reason = refused_accept(seed, |receiver| {
            on_template(
                accept_exercise(INSTRUCTION_CID, receiver, extra_args()),
                template,
            )
        })
        .await;

        match reason {
            Refusal::Mismatch {
                field,
                expected,
                found,
            } => {
                assert_eq!(field, "template");
                assert_eq!(expected, AMULET_TRANSFER_INSTRUCTION_TEMPLATE);
                assert_eq!(found, template);
            }
            other => panic!("{template}: expected a template mismatch, got {other}"),
        }
    }
}

/// The package id of the `splice-amulet` package in these fixtures. The
/// factory of [`exercise`] and the template [`on_template`] gives Canton
/// Coin's instruction both use it.
const AMULET_PACKAGE_ID: &str = "splice-amulet-package-id";

/// Canton Coin's instruction template, pinned to `package_id`.
fn pinned_to(package_id: &str) -> String {
    let (_, qualified) = AMULET_TRANSFER_INSTRUCTION_TEMPLATE
        .split_once(':')
        .unwrap_or_default();
    format!("{package_id}:{qualified}")
}

/// `root` on a package with the same name, module and entity, and another
/// package id.
fn on_look_alike(root: Exercise) -> Exercise {
    let template_id = root.template_id.map(|id| Identifier {
        package_id: "look-alike-package-id".to_string(),
        ..id
    });
    Exercise {
        template_id,
        ..root
    }
}

/// Run an accept of `INSTRUCTION_CID` that expects `template`, against a
/// participant that prepares `root`, and return the prepare body. Panics
/// unless the wallet signed the transaction's hash and executed it once.
async fn signed_accept(seed: u8, template: &str, root: impl FnOnce(&str) -> Exercise) -> Value {
    let key = ExternalKeyPair::from_seed([seed; 32]);
    let receiver = key.party_id("bob");
    let transaction = prepared(root(&receiver), &receiver);
    let server = MockServer::start().await;
    stub_accept(&server, honestly(transaction.clone())).await;

    let result = accept_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &receiver,
        INSTRUCTION_CID,
        template,
    )
    .await;

    if let Err(e) = result {
        panic!("{template}: the accept must be signed: {e}");
    }
    let hash = hash_of(&for_command(transaction, &sent_command_id(&server).await));
    let executes = bodies(&server, EXECUTE_PATH).await;
    assert_eq!(executes.len(), 1, "{template}: one execute");
    let (_, _, signature) = signature_of(&executes[0]);
    assert!(
        verifying_key(&key).verify(&hash, &signature).is_ok(),
        "{template}: the signature must cover the transaction's hash"
    );
    bodies(&server, PREPARE_PATH).await.remove(0)
}

/// A pinned template binds the package id, so a look-alike package with the
/// same name is refused. The refusal shows the id the root really has.
#[tokio::test]
async fn an_accept_pinned_to_another_package_id_is_not_signed() {
    let pinned = pinned_to(AMULET_PACKAGE_ID);

    let reason = refused_accept_expecting(53, &pinned, |receiver| {
        on_look_alike(accept_exercise(INSTRUCTION_CID, receiver, extra_args()))
    })
    .await;

    match reason {
        Refusal::Mismatch {
            field,
            expected,
            found,
        } => {
            assert_eq!(field, "template");
            assert_eq!(expected, pinned);
            assert_eq!(found, pinned_to("look-alike-package-id"));
        }
        other => panic!("expected a template mismatch, got {other}"),
    }
}

/// The pinned accept asks the participant to prefer the pinned package, so
/// an upgraded package does not replace it.
#[tokio::test]
async fn an_accept_pinned_to_the_instructions_package_id_is_signed() {
    let prepare = signed_accept(54, &pinned_to(AMULET_PACKAGE_ID), |receiver| {
        accept_exercise(INSTRUCTION_CID, receiver, extra_args())
    })
    .await;

    assert_eq!(
        prepare["packageIdSelectionPreference"],
        json!([AMULET_PACKAGE_ID])
    );
}

/// The name form does not pin the package id.
#[tokio::test]
async fn an_accept_by_package_name_is_signed_on_a_look_alike_package() {
    let prepare = signed_accept(55, AMULET_TRANSFER_INSTRUCTION_TEMPLATE, |receiver| {
        on_look_alike(accept_exercise(INSTRUCTION_CID, receiver, extra_args()))
    })
    .await;

    assert_eq!(prepare["packageIdSelectionPreference"], json!([]));
}

/// With smart contract upgrade, the participant may interpret the factory
/// with another package of the same name. A send pinned to the factory's
/// package id refuses it.
#[tokio::test]
async fn a_send_pinned_to_another_package_id_is_not_signed() {
    let pinned = factory_pinned_to(AMULET_PACKAGE_ID);

    let reason = refused_send_against(56, Some(&pinned), |sender| {
        honestly(prepared(on_look_alike(faithful_root(sender)), sender))
    })
    .await;

    match reason {
        Refusal::Mismatch {
            field,
            expected,
            found,
        } => {
            assert_eq!(field, "template");
            assert_eq!(expected, pinned);
            assert_eq!(found, factory_pinned_to("look-alike-package-id"));
        }
        other => panic!("expected a template mismatch, got {other}"),
    }
}

#[tokio::test]
async fn a_send_pinned_to_the_factorys_package_id_is_signed_and_prefers_that_package() {
    let key = ExternalKeyPair::from_seed([57u8; 32]);
    let sender = key.party_id("alice");
    let pinned = factory_pinned_to(AMULET_PACKAGE_ID);
    let server = MockServer::start().await;
    stub_send(&server, &sender, honestly(faithful_send(&sender))).await;
    let args = TransferArgs {
        factory_template: Some(pinned.clone()),
        ..send_args(&sender)
    };

    let receipt =
        match send_transfer(&ledger_for(&server), &registry_for(&server), &key, &args).await {
            Ok(receipt) => receipt,
            Err(e) => panic!("a send pinned to the factory's package must be signed: {e}"),
        };

    let prepare = &bodies(&server, PREPARE_PATH).await[0];
    assert_eq!(
        prepare["packageIdSelectionPreference"],
        json!([AMULET_PACKAGE_ID])
    );
    assert_eq!(receipt.factory_template, Some(pinned));
    let hash = hash_of(&for_command(
        faithful_send(&sender),
        &sent_command_id(&server).await,
    ));
    let executes = bodies(&server, EXECUTE_PATH).await;
    assert_eq!(executes.len(), 1, "one execute");
    let (_, _, signature) = signature_of(&executes[0]);
    assert!(
        verifying_key(&key).verify(&hash, &signature).is_ok(),
        "the signature must cover the transaction's hash"
    );
}

/// The registry's context and the empty `meta` the wallet sends are signed
/// as sent. A participant that changes them is refused.
#[tokio::test]
async fn an_accept_with_extra_args_other_than_the_wallet_sent_is_not_signed() {
    let other_rules = extra_args_with(vec![("amulet-rules", any_contract_id("00c2"))]);
    let added = extra_args_with(vec![
        ("amulet-rules", any_contract_id("00c1")),
        ("open-round", any_contract_id("00c3")),
    ]);
    let removed = extra_args_with(Vec::new());
    let filled_meta = with_field(extra_args(), "meta", Some(filled_metadata()));

    for (seed, case, extra_args) in [
        (31, "a changed context entry", other_rules),
        (32, "an added context entry", added),
        (33, "a removed context entry", removed),
        (34, "a filled meta", filled_meta),
    ] {
        let reason = refused_accept(seed, |receiver| {
            accept_exercise(INSTRUCTION_CID, receiver, extra_args)
        })
        .await;

        match reason {
            Refusal::Mismatch {
                field,
                expected,
                found,
            } => {
                assert_eq!(field, "extraArgs", "{case}");
                assert_eq!(
                    serde_json::from_str::<Value>(&expected).ok(),
                    Some(json!({ "context": context_data(), "meta": { "values": {} } })),
                    "{case}: the refusal shows what the wallet sent"
                );
                assert_ne!(expected, found, "{case}");
            }
            other => panic!("{case}: expected an extraArgs mismatch, got {other}"),
        }
    }
}

/// A send compares its own `extraArgs` and `transfer.meta` too.
#[tokio::test]
async fn a_send_with_extra_args_or_meta_other_than_the_wallet_sent_is_not_signed() {
    let cases: [(u8, &str, &str, DamlValue); 2] = [
        (
            35,
            "extraArgs",
            "extraArgs.context.values",
            text_map_of(vec![("amulet-rules", any_contract_id("00c2"))]),
        ),
        (36, "transfer.meta", "transfer.meta", filled_metadata()),
    ];
    for (seed, expected_field, path, value) in cases {
        let reason = refused_send(seed, |sender| {
            exercise(
                FACTORY_ID,
                "TransferFactory",
                TRANSFER_CHOICE,
                &[sender],
                with_field(transfer_value(&faithful(sender)), path, Some(value)),
            )
        })
        .await;

        match reason {
            Refusal::Mismatch { field, .. } => assert_eq!(field, expected_field),
            other => panic!("{path}: expected a mismatch, got {other}"),
        }
    }
}

/// The hash covers the command id, and the ledger finds a completion by it.
/// A transaction prepared under another command id is refused.
#[tokio::test]
async fn a_send_with_another_command_id_is_not_signed() {
    let key = ExternalKeyPair::from_seed([47u8; 32]);
    let sender = key.party_id("alice");
    let transaction = for_command(faithful_send(&sender), "another-command");
    let response = prepare_response(&transaction, &hash_of(&transaction));
    let server = MockServer::start().await;
    stub_send(&server, &sender, move |_: &str| response.clone()).await;

    let result = send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await;

    match result {
        Err(Error::TransactionRefused {
            reason:
                Refusal::Mismatch {
                    field,
                    expected,
                    found,
                },
            ..
        }) => {
            assert_eq!(field, "command id");
            assert_eq!(expected, sent_command_id(&server).await);
            assert_eq!(found, "another-command");
        }
        Err(other) => panic!("expected a command id mismatch, got {other}"),
        Ok(_) => panic!("a transaction for another command must not be signed"),
    }
    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
}

/// The execute is the step that commits. When its request left the wallet
/// but no answer came back, the transfer may have happened, and the error
/// says so.
#[tokio::test]
async fn an_execute_without_an_answer_has_an_unknown_outcome() {
    let key = ExternalKeyPair::from_seed([37u8; 32]);
    let sender = key.party_id("alice");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(EXECUTE_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "updateId": "update-1", "completionOffset": 43 }))
                .set_delay(Duration::from_secs(3)),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    stub_send(&server, &sender, honestly(faithful_send(&sender))).await;
    let ledger = match LedgerClient::with_options(
        format!("{uri}{LEDGER_PREFIX}", uri = server.uri()),
        None,
        "wallet-user",
        ClientOptions {
            timeout: Duration::from_secs(1),
            ..ClientOptions::default()
        },
    ) {
        Ok(client) => client,
        Err(e) => panic!("ledger client must build: {e}"),
    };

    let error =
        match send_transfer(&ledger, &registry_for(&server), &key, &send_args(&sender)).await {
            Ok(_) => panic!("an execute without an answer must not look like a success"),
            Err(e) => e,
        };

    let wallet_command_id = sent_command_id(&server).await;
    let transaction = for_command(faithful_send(&sender), &wallet_command_id);
    let executes = bodies(&server, EXECUTE_PATH).await;
    assert_eq!(executes.len(), 1, "the wallet does not resend the execute");
    match &error {
        Error::ExecuteOutcomeUnknown {
            host,
            command_id,
            submission_id,
            prepared_transaction_hash,
            ..
        } => {
            assert!(host.ends_with(LEDGER_PREFIX), "{host}");
            assert_eq!(
                prepared_transaction_hash,
                &STANDARD.encode(hash_of(&transaction))
            );
            assert_eq!(command_id, &wallet_command_id);
            assert_eq!(
                Some(submission_id.as_str()),
                executes[0]["submissionId"].as_str()
            );
        }
        other => panic!("expected an unknown outcome, got {other}"),
    }
    assert!(error.to_string().contains("may have committed"), "{error}");
}

/// A gateway in front of the participant answers 504 when it stops waiting.
/// The participant may still execute, so the outcome is unknown.
#[tokio::test]
async fn an_execute_answered_with_a_gateway_timeout_has_an_unknown_outcome() {
    let key = ExternalKeyPair::from_seed([52u8; 32]);
    let sender = key.party_id("alice");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(EXECUTE_PATH))
        .respond_with(ResponseTemplate::new(504).set_body_string("upstream request timeout"))
        .with_priority(1)
        .mount(&server)
        .await;
    stub_send(&server, &sender, honestly(faithful_send(&sender))).await;

    let error = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(_) => panic!("a 504 from the execute must not look like a success"),
        Err(e) => e,
    };

    let wallet_command_id = sent_command_id(&server).await;
    let transaction = for_command(faithful_send(&sender), &wallet_command_id);
    let executes = bodies(&server, EXECUTE_PATH).await;
    assert_eq!(executes.len(), 1, "the wallet does not resend the execute");
    match &error {
        Error::ExecuteOutcomeUnknown {
            host,
            command_id,
            submission_id,
            prepared_transaction_hash,
            source,
        } => {
            assert!(host.ends_with(LEDGER_PREFIX), "{host}");
            assert_eq!(command_id, &wallet_command_id);
            assert_eq!(
                Some(submission_id.as_str()),
                executes[0]["submissionId"].as_str()
            );
            assert_eq!(
                prepared_transaction_hash,
                &STANDARD.encode(hash_of(&transaction))
            );
            assert!(source.is_status(504), "{source}");
        }
        other => panic!("expected an unknown outcome, got {other}"),
    }
    assert!(error.to_string().contains("may have committed"), "{error}");
}

/// Canton's JSON API answers 500 for an internal gRPC error, which can come
/// after the participant accepted the submission. The outcome is unknown.
#[tokio::test]
async fn an_execute_answered_with_an_internal_error_has_an_unknown_outcome() {
    let key = ExternalKeyPair::from_seed([58u8; 32]);
    let sender = key.party_id("alice");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(EXECUTE_PATH))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "code": "LEDGER_API_INTERNAL_ERROR",
            "cause": "An error occurred. Please contact the operator.",
            "context": {},
            "errorCategory": 4,
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    stub_send(&server, &sender, honestly(faithful_send(&sender))).await;

    let error = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(_) => panic!("a 500 from the execute must not look like a success"),
        Err(e) => e,
    };

    let wallet_command_id = sent_command_id(&server).await;
    let transaction = for_command(faithful_send(&sender), &wallet_command_id);
    let executes = bodies(&server, EXECUTE_PATH).await;
    assert_eq!(executes.len(), 1, "the wallet does not resend the execute");
    match &error {
        Error::ExecuteOutcomeUnknown {
            host,
            command_id,
            submission_id,
            prepared_transaction_hash,
            source,
        } => {
            assert!(host.ends_with(LEDGER_PREFIX), "{host}");
            assert_eq!(command_id, &wallet_command_id);
            assert_eq!(
                Some(submission_id.as_str()),
                executes[0]["submissionId"].as_str()
            );
            assert_eq!(
                prepared_transaction_hash,
                &STANDARD.encode(hash_of(&transaction))
            );
            assert!(source.is_status(500), "{source}");
        }
        other => panic!("expected an unknown outcome, got {other}"),
    }
    assert!(error.to_string().contains("may have committed"), "{error}");
}

/// How the stub participant answers the execute.
#[derive(Clone, Copy, Debug)]
enum ExecuteAnswer {
    /// 200, part of the body, then silence until the wallet gives up.
    Stalls,
    /// 200, part of the body, then the connection closes.
    CutShort,
    /// 200 with a whole body that is not JSON.
    NotJson,
    /// 200 with a body larger than the wallet reads.
    OverTheCap,
}

impl ExecuteAnswer {
    /// Whether `error` is the read error this answer causes.
    fn explains(self, error: &Error) -> bool {
        match (self, error) {
            (Self::Stalls, Error::Decode { source, .. }) => source.is_timeout(),
            (Self::CutShort, Error::Decode { source, .. }) => !source.is_timeout(),
            (Self::NotJson, Error::Json { .. }) => true,
            (Self::OverTheCap, Error::ResponseTooLarge { .. }) => true,
            _ => false,
        }
    }
}

/// The body cap of the ledger client that reads these answers. The prepare
/// answer fits under it.
const SMALL_CAP: usize = 64 * 1024;

/// One HTTP/1.1 request from `stream`: its path and its JSON body.
fn read_request(stream: &TcpStream) -> Option<(String, Value)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let request_path = line.split_whitespace().nth(1)?.to_string();
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).ok()?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((request_path, serde_json::from_slice(&body).ok()?))
}

/// A participant for one accept by `receiver`, on a plain socket, because
/// the mock server cannot stall or cut a body. It prepares the accept for
/// the command id the wallet sends, and answers the execute as `answer`
/// says. It passes on each request it reads.
fn raw_participant(
    receiver: String,
    answer: ExecuteAnswer,
) -> (String, mpsc::Receiver<(String, Value)>) {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(e) => panic!("the stub participant must bind: {e}"),
    };
    let base_url = match listener.local_addr() {
        Ok(address) => format!("http://{address}{LEDGER_PREFIX}"),
        Err(e) => panic!("the stub participant must have an address: {e}"),
    };
    let (requests, received) = mpsc::channel();
    std::thread::spawn(move || {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n";
        for stream in listener.incoming().take(2) {
            let Ok(mut stream) = stream else { return };
            let Some((request_path, body)) = read_request(&stream) else {
                return;
            };
            let command_id = body["commandId"].as_str().unwrap_or_default().to_string();
            let is_prepare = request_path == PREPARE_PATH;
            let _ = requests.send((request_path, body));
            if is_prepare {
                let transaction =
                    for_command(accept_transaction(INSTRUCTION_CID, &receiver), &command_id);
                let json = prepare_response(&transaction, &hash_of(&transaction)).to_string();
                let _ = write!(stream, "{head}Content-Length: {}\r\n\r\n{json}", json.len());
                continue;
            }
            let _ = match answer {
                ExecuteAnswer::Stalls | ExecuteAnswer::CutShort => write!(
                    stream,
                    "{head}Content-Length: 64\r\n\r\n{{\"updateId\": \"upd"
                ),
                ExecuteAnswer::NotJson => {
                    write!(stream, "{head}Content-Length: 8\r\n\r\nnot json")
                }
                ExecuteAnswer::OverTheCap => {
                    write!(stream, "{head}Content-Length: {}\r\n\r\n", SMALL_CAP + 1)
                }
            };
            if !matches!(answer, ExecuteAnswer::CutShort) {
                // Keep the connection open until the wallet closes it.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let _ = stream.read(&mut [0; 1]);
            }
        }
    });
    (base_url, received)
}

/// The participant may have committed the transaction even when its answer
/// to the execute cannot be read. The error keeps the ids and the hash that
/// a completion lookup needs.
#[tokio::test]
async fn an_execute_whose_answer_cannot_be_read_has_an_unknown_outcome() {
    for (seed, answer) in [
        (48u8, ExecuteAnswer::Stalls),
        (49, ExecuteAnswer::CutShort),
        (50, ExecuteAnswer::NotJson),
        (51, ExecuteAnswer::OverTheCap),
    ] {
        let key = ExternalKeyPair::from_seed([seed; 32]);
        let receiver = key.party_id("bob");
        let registry = MockServer::start().await;
        stub_accept_context(&registry).await;
        let (base_url, requests) = raw_participant(receiver.clone(), answer);
        let ledger = match LedgerClient::with_options(
            base_url,
            None,
            "wallet-user",
            ClientOptions {
                timeout: Duration::from_millis(500),
                max_response_bytes: SMALL_CAP,
                ..ClientOptions::default()
            },
        ) {
            Ok(client) => client,
            Err(e) => panic!("ledger client must build: {e}"),
        };

        let result = accept_transfer(
            &ledger,
            &registry_for(&registry),
            &key,
            &receiver,
            INSTRUCTION_CID,
            AMULET_TRANSFER_INSTRUCTION_TEMPLATE,
        )
        .await;

        let requests: Vec<(String, Value)> = requests.try_iter().collect();
        let [(prepare_path, prepare), (execute_path, execute)] = requests.as_slice() else {
            panic!(
                "{answer:?}: expected one prepare and one execute, got {} requests",
                requests.len()
            );
        };
        assert_eq!(prepare_path, PREPARE_PATH, "{answer:?}");
        assert_eq!(execute_path, EXECUTE_PATH, "{answer:?}");
        let transaction = for_command(
            accept_transaction(INSTRUCTION_CID, &receiver),
            prepare["commandId"].as_str().unwrap_or_default(),
        );
        let error = match result {
            Ok(_) => panic!("{answer:?}: an unreadable answer must not look like a success"),
            Err(e) => e,
        };
        match &error {
            Error::ExecuteOutcomeUnknown {
                host,
                command_id,
                submission_id,
                prepared_transaction_hash,
                source,
            } => {
                assert!(host.ends_with(LEDGER_PREFIX), "{answer:?}: {host}");
                assert_eq!(
                    Some(command_id.as_str()),
                    prepare["commandId"].as_str(),
                    "{answer:?}"
                );
                assert_eq!(
                    Some(submission_id.as_str()),
                    execute["submissionId"].as_str(),
                    "{answer:?}"
                );
                assert_eq!(
                    prepared_transaction_hash,
                    &STANDARD.encode(hash_of(&transaction)),
                    "{answer:?}"
                );
                assert!(
                    answer.explains(source),
                    "{answer:?}: the source is {source}"
                );
            }
            other => panic!("{answer:?}: expected an unknown outcome, got {other}"),
        }
        assert!(error.to_string().contains("may have committed"), "{error}");
    }
}

/// The instruction id goes into the registry's URL path, so anything other
/// than a contract id is refused before any call.
#[tokio::test]
async fn an_instruction_id_that_is_not_a_contract_id_is_refused_before_any_call() {
    let key = ExternalKeyPair::from_seed([38u8; 32]);
    let receiver = key.party_id("bob");
    // No stubs: any call would be recorded, and the test counts them.
    let server = MockServer::start().await;
    let ledger = ledger_for(&server);
    let registry = registry_for(&server);

    for bad in [
        "00b1".to_string(),
        format!("{INSTRUCTION_CID}/../../transfer-factory"),
        INSTRUCTION_CID.to_uppercase(),
        format!("{INSTRUCTION_CID}0"),
    ] {
        let accepted = accept_transfer(
            &ledger,
            &registry,
            &key,
            &receiver,
            &bad,
            AMULET_TRANSFER_INSTRUCTION_TEMPLATE,
        )
        .await;
        assert!(
            matches!(&accepted, Err(Error::InvalidContractId { contract_id }) if *contract_id == bad),
            "{bad}: expected InvalidContractId, got {:?}",
            accepted.err()
        );
        let context = registry.accept_context(&bad).await;
        assert!(
            matches!(&context, Err(Error::InvalidContractId { contract_id }) if *contract_id == bad),
            "{bad}: expected InvalidContractId, got {:?}",
            context.err()
        );
    }

    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0,
        "nothing is sent"
    );
}

#[test]
fn the_clients_refuse_plain_http_to_another_host_unless_allowed() {
    let allowed = ClientOptions {
        allow_plain_http: true,
        ..ClientOptions::default()
    };

    assert!(matches!(
        LedgerClient::new(
            "http://ledger.example.com/api/json-api",
            None,
            "wallet-user"
        ),
        Err(Error::InsecureUrl { .. })
    ));
    assert!(matches!(
        RegistryClient::new("http://scan.example.com", None),
        Err(Error::InsecureUrl { .. })
    ));
    assert!(
        LedgerClient::with_options(
            "http://ledger.example.com/api/json-api",
            None,
            "wallet-user",
            allowed.clone(),
        )
        .is_ok()
    );
    assert!(RegistryClient::with_options("http://scan.example.com", None, allowed).is_ok());
}

/// A 307 or 308 would make the client send the prepare body again, to a URL
/// the plain `http` rule never checked. The client follows no redirect.
#[tokio::test]
async fn a_send_with_a_redirected_prepare_is_not_signed() {
    for (seed, status) in [(45u8, 307u16), (46, 308)] {
        let key = ExternalKeyPair::from_seed([seed; 32]);
        let sender = key.party_id("alice");
        let elsewhere = MockServer::start().await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PREPARE_PATH))
            .respond_with(ResponseTemplate::new(status).insert_header(
                "location",
                format!("{uri}{PREPARE_PATH}", uri = elsewhere.uri()),
            ))
            .with_priority(1)
            .mount(&server)
            .await;
        stub_send(&server, &sender, honestly(faithful_send(&sender))).await;

        let result = send_transfer(
            &ledger_for(&server),
            &registry_for(&server),
            &key,
            &send_args(&sender),
        )
        .await;

        match result {
            Err(Error::Api {
                path,
                status: found,
                ..
            }) => {
                assert_eq!(path, "/v2/interactive-submission/prepare");
                assert_eq!(found, status);
            }
            Err(other) => panic!("{status}: expected the redirect as an API error, got {other}"),
            Ok(_) => panic!("{status}: a redirected prepare must not be signed"),
        }
        assert_eq!(
            count(&server, EXECUTE_PATH).await,
            0,
            "{status}: nothing is executed"
        );
        assert_eq!(
            elsewhere
                .received_requests()
                .await
                .unwrap_or_default()
                .len(),
            0,
            "{status}: the redirect is not followed"
        );
    }
}

// ---------------------------------------------------------------------------
// Transactions of a shape the check does not accept.
// ---------------------------------------------------------------------------

/// The root a faithful send prepares for `sender`.
fn faithful_root(sender: &str) -> Exercise {
    exercise(
        FACTORY_ID,
        "TransferFactory",
        TRANSFER_CHOICE,
        &[sender],
        transfer_value(&faithful(sender)),
    )
}

fn assert_shape(reason: Refusal, wanted: &str) {
    match reason {
        Refusal::Shape(found) => assert_eq!(found, wanted),
        other => panic!("expected the shape refusal \"{wanted}\", got {other}"),
    }
}

/// A second root could do anything, and the check reads only one.
#[tokio::test]
async fn a_transaction_with_two_roots_is_not_signed() {
    let reason = refused_send_of(39, |sender| {
        let mut transaction = prepared(faithful_root(sender), sender);
        if let Some(tree) = transaction.transaction.as_mut() {
            tree.roots.push("1".to_string());
            tree.nodes.push(Node {
                node_id: "1".to_string(),
                versioned_node: Some(VersionedNode::V1(v1::Node {
                    node_type: Some(NodeType::Exercise(faithful_root(sender))),
                })),
            });
            tree.node_seeds.push(NodeSeed {
                node_id: 1,
                seed: vec![8; 32],
            });
        }
        transaction
    })
    .await;

    assert_shape(reason, "it has 2 root nodes, not one");
}

#[tokio::test]
async fn a_transaction_whose_root_is_a_create_is_not_signed() {
    let reason = refused_send_of(40, |sender| {
        let create = v1::Create {
            lf_version: "2.1".to_string(),
            contract_id: "00e1".to_string(),
            package_name: "splice-amulet".to_string(),
            template_id: Some(Identifier {
                package_id: "amulet-package-id".to_string(),
                module_name: "Splice.Amulet".to_string(),
                entity_name: "Amulet".to_string(),
            }),
            argument: Some(record(vec![("owner", party(MALLORY))])),
            signatories: vec![sender.to_string()],
            stakeholders: vec![sender.to_string(), MALLORY.to_string()],
            ..v1::Create::default()
        };
        prepared_node(NodeType::Create(create), sender)
    })
    .await;

    assert_shape(reason, "its root node is not an exercise");
}

/// The hash covers the choice argument, so without one the wallet cannot
/// recompute the hash, and it refuses there.
#[tokio::test]
async fn a_root_exercise_without_a_choice_argument_is_not_signed() {
    let reason = refused_send(41, |sender| Exercise {
        chosen_value: None,
        ..faithful_root(sender)
    })
    .await;

    match reason {
        Refusal::Unverifiable(why) => assert!(why.contains("chosen value"), "{why}"),
        other => panic!("expected an unverifiable hash, got {other}"),
    }
}

#[tokio::test]
async fn a_send_whose_argument_lacks_the_receiver_is_not_signed() {
    let reason = refused_send(42, |sender| {
        exercise(
            FACTORY_ID,
            "TransferFactory",
            TRANSFER_CHOICE,
            &[sender],
            with_field(transfer_value(&faithful(sender)), "transfer.receiver", None),
        )
    })
    .await;

    assert_shape(reason, "its choice argument has no transfer.receiver");
}

/// An exercise without an interface is a direct template choice, not the
/// Token Standard choice the wallet asked for.
#[tokio::test]
async fn a_root_exercise_without_an_interface_is_not_signed() {
    let reason = refused_send(43, |sender| Exercise {
        interface_id: None,
        ..faithful_root(sender)
    })
    .await;

    match reason {
        Refusal::Mismatch {
            field,
            expected,
            found,
        } => {
            assert_eq!(field, "interface");
            assert_eq!(
                expected,
                "Splice.Api.Token.TransferInstructionV1:TransferFactory"
            );
            assert_eq!(found, "");
        }
        other => panic!("expected an interface mismatch, got {other}"),
    }
}

fn v3_hash_of(transaction: &PreparedTransaction) -> Vec<u8> {
    match compute_prepared_transaction_hash(HashingScheme::V3, transaction) {
        Ok(hash) => hash,
        Err(e) => panic!("the fixture must hash with V3: {e}"),
    }
}

/// The prepare response for `transaction`, naming V3 and carrying `hash`.
fn named_v3(transaction: &PreparedTransaction, hash: &[u8]) -> Value {
    let mut response = prepare_response(transaction, hash);
    response["hashingSchemeVersion"] = json!("HASHING_SCHEME_VERSION_V3");
    response
}

/// Canton prepares with V3 when asked. The wallet recomputes the hash with
/// the scheme the prepare names, signs that hash, and echoes the scheme.
#[tokio::test]
async fn a_transaction_prepared_with_hashing_scheme_v3_is_checked_and_signed_with_v3() {
    let key = ExternalKeyPair::from_seed([44u8; 32]);
    let sender = key.party_id("alice");
    let transaction = prepared(faithful_root(&sender), &sender);

    let server = MockServer::start().await;
    let participant = preparing(transaction.clone(), |transaction| {
        named_v3(transaction, &v3_hash_of(transaction))
    });
    stub_send(&server, &sender, participant).await;
    let receipt = match send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await
    {
        Ok(receipt) => receipt,
        Err(e) => panic!("a V3 transaction must be signed: {e}"),
    };

    let sent = for_command(transaction.clone(), &sent_command_id(&server).await);
    let v3 = v3_hash_of(&sent);
    assert_ne!(
        v3,
        hash_of(&sent),
        "the test needs a transaction the two schemes hash differently"
    );
    assert_eq!(receipt.prepared_transaction_hash, STANDARD.encode(&v3));
    let execute = &bodies(&server, EXECUTE_PATH).await[0];
    assert_eq!(execute["hashingSchemeVersion"], "HASHING_SCHEME_VERSION_V3");
    let (_, _, signature) = signature_of(execute);
    assert!(
        verifying_key(&key).verify(&v3, &signature).is_ok(),
        "the signature must cover the V3 hash"
    );

    // Named V3 but carrying the V2 hash: the wallet hashed with V3.
    let server = MockServer::start().await;
    let participant = preparing(transaction, |transaction| {
        named_v3(transaction, &hash_of(transaction))
    });
    stub_send(&server, &sender, participant).await;
    let result = send_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &send_args(&sender),
    )
    .await;
    assert!(
        matches!(
            result,
            Err(Error::TransactionRefused {
                reason: Refusal::HashMismatch { .. },
                ..
            })
        ),
        "a V2 hash named V3 must be refused"
    );
    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
}
