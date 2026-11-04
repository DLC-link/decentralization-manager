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
        Identifier, List, Record, RecordField, TextMap, Value as DamlValue,
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
use decman_wallet::{
    Error, ExternalKeyPair, LedgerClient, Refusal, RegistryClient, TransferArgs, accept_transfer,
    send_transfer,
    transfer::{
        ACCEPT_CHOICE, HOLDING_INTERFACE_ID, TRANSFER_CHOICE, TRANSFER_FACTORY_INTERFACE_ID,
        TRANSFER_INSTRUCTION_INTERFACE_ID,
    },
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use prost::Message;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
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

fn instruction_view(sender: &str, receiver: &str, amount: &str) -> Value {
    json!({
        "originalInstructionCid": null,
        "transfer": {
            "sender": sender,
            "receiver": receiver,
            "amount": amount,
            "instrumentId": { "admin": DSO, "id": "Amulet" },
            "requestedAt": "2026-11-04T10:00:00Z",
            "executeBefore": "2026-11-05T10:00:00Z",
            "inputHoldingCids": [],
            "meta": { "values": {} },
        },
        "status": { "tag": "TransferPendingReceiverAcceptance", "value": {} },
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
    stub_acs(
        &server,
        vec![
            acs_entry(
                "00b1",
                interface,
                instruction_view(SENDER, RECEIVER, "1.2500000000"),
            ),
            // The receiver's own outgoing offer: not one it can accept.
            acs_entry(
                "00b2",
                interface,
                instruction_view(RECEIVER, SENDER, "3.0000000000"),
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
    assert_eq!(pending[0].sender, SENDER);
    assert_eq!(pending[0].receiver, RECEIVER);
    assert_eq!(pending[0].amount, "1.2500000000");
    assert_eq!(pending[0].instrument_admin, DSO);
    assert_eq!(pending[0].instrument_id, "Amulet");

    let acs = bodies(
        &server,
        &format!("{LEDGER_PREFIX}/v2/state/active-contracts"),
    )
    .await;
    let filter = &acs[0]["eventFormat"]["filtersByParty"][RECEIVER]["cumulative"][0]["identifierFilter"]
        ["InterfaceFilter"]["value"];
    assert_eq!(filter["interfaceId"], TRANSFER_INSTRUCTION_INTERFACE_ID);
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
const INSTRUCTION_CID: &str = "00b1";
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

fn empty_metadata() -> DamlValue {
    record(vec![("values", daml(Sum::TextMap(TextMap::default())))])
}

/// `ExtraArgs` as the ledger writes it, carrying one context entry.
fn extra_args() -> DamlValue {
    let entry = text_map::Entry {
        key: "amulet-rules".to_string(),
        value: Some(daml(Sum::Text("00c1".to_string()))),
    };
    record(vec![
        (
            "context",
            record(vec![(
                "values",
                daml(Sum::TextMap(TextMap {
                    entries: vec![entry],
                })),
            )]),
        ),
        ("meta", empty_metadata()),
    ])
}

/// What the ledger puts in the transaction for a transfer. The amount is in
/// the ledger's own form, with ten decimals.
struct OnLedger<'a> {
    sender: &'a str,
    receiver: &'a str,
    amount: &'a str,
    instrument_id: &'a str,
}

/// The `TransferFactory_Transfer` choice argument as a Daml value.
fn transfer_value(transfer: &OnLedger<'_>) -> DamlValue {
    record(vec![
        ("expectedAdmin", party(DSO)),
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
                ("requestedAt", daml(Sum::Timestamp(1_793_786_400_000_000))),
                ("executeBefore", daml(Sum::Timestamp(1_793_872_800_000_000))),
                (
                    "inputHoldingCids",
                    daml(Sum::List(List {
                        elements: vec![daml(Sum::ContractId("00a1".to_string()))],
                    })),
                ),
                ("meta", empty_metadata()),
            ]),
        ),
        ("extraArgs", extra_args()),
    ])
}

/// The root exercise of a Token Standard transaction, through `interface`.
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
            package_id: "amulet-package-id".to_string(),
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
    PreparedTransaction {
        transaction: Some(DamlTransaction {
            version: "2.1".to_string(),
            roots: vec!["0".to_string()],
            nodes: vec![Node {
                node_id: "0".to_string(),
                versioned_node: Some(VersionedNode::V1(v1::Node {
                    node_type: Some(NodeType::Exercise(root)),
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

/// A faithful send transaction for `sender`, and the prepare response for it.
fn faithful_send(sender: &str) -> (PreparedTransaction, Value) {
    let transaction = prepared(
        exercise(
            FACTORY_ID,
            "TransferFactory",
            TRANSFER_CHOICE,
            &[sender],
            transfer_value(&OnLedger {
                sender,
                receiver: RECEIVER,
                amount: "1.2500000000",
                instrument_id: "Amulet",
            }),
        ),
        sender,
    );
    let response = prepare_response(&transaction, &hash_of(&transaction));
    (transaction, response)
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
                "disclosedContracts": disclosed(),
            },
        })))
        .mount(server)
        .await;
}

async fn stub_prepare_and_execute(server: &MockServer, prepare_response: Value) {
    Mock::given(method("POST"))
        .and(path(PREPARE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(prepare_response))
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
async fn stub_send(server: &MockServer, sender: &str, prepare_response: Value) {
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
    stub_prepare_and_execute(server, prepare_response).await;
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
    let (transaction, response) = faithful_send(&sender);
    let hash = hash_of(&transaction);
    let server = MockServer::start().await;
    stub_send(&server, &sender, response.clone()).await;

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

    assert_eq!(receipt.update_id, "update-1");
    assert_eq!(receipt.completion_offset, 43);
    assert_eq!(receipt.factory_id, FACTORY_ID);
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
    assert_eq!(prepare["disclosedContracts"], disclosed());
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
        response["preparedTransaction"]
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
    stub_prepare_and_execute(&server, faithful_send(&sender).1).await;

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
    let (shown, _) = faithful_send(&sender);
    let other = prepared(
        exercise(
            FACTORY_ID,
            "TransferFactory",
            TRANSFER_CHOICE,
            &[&sender],
            transfer_value(&OnLedger {
                sender: &sender,
                receiver: MALLORY,
                amount: "1.2500000000",
                instrument_id: "Amulet",
            }),
        ),
        &sender,
    );
    let server = MockServer::start().await;
    stub_send(&server, &sender, prepare_response(&shown, &hash_of(&other))).await;

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
                sender: &sender,
                receiver: MALLORY,
                amount: "1.2500000000",
                instrument_id: "Amulet",
            }),
        ),
        &sender,
    );
    let server = MockServer::start().await;
    stub_send(
        &server,
        &sender,
        prepare_response(&redirected, &hash_of(&redirected)),
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
    let faithful = OnLedger {
        sender: &sender,
        receiver: RECEIVER,
        amount: "1.2500000000",
        instrument_id: "Amulet",
    };
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
                    ..faithful
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
                    ..faithful
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
                    ..faithful
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
                transfer_value(&faithful),
            ),
        ),
        (
            "contract id",
            exercise(
                "00f2",
                "TransferFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&faithful),
            ),
        ),
        (
            "choice",
            exercise(
                FACTORY_ID,
                "TransferFactory",
                "TransferFactory_PublicFetch",
                &[&sender],
                transfer_value(&faithful),
            ),
        ),
        (
            "interface",
            exercise(
                FACTORY_ID,
                "AllocationFactory",
                TRANSFER_CHOICE,
                &[&sender],
                transfer_value(&faithful),
            ),
        ),
    ];

    for (expected_field, root) in tampered {
        let transaction = prepared(root, &sender);
        let server = MockServer::start().await;
        stub_send(
            &server,
            &sender,
            prepare_response(&transaction, &hash_of(&transaction)),
        )
        .await;

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
    let (_, mut response) = faithful_send(&sender);
    response["hashingSchemeVersion"] = json!("HASHING_SCHEME_VERSION_V4");
    let server = MockServer::start().await;
    stub_send(&server, &sender, response).await;

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

/// A ledger and registry ready for `receiver` to accept `INSTRUCTION_CID`.
async fn stub_accept(server: &MockServer, prepare_response: Value) {
    Mock::given(method("POST"))
        .and(path(accept_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choiceContextData": context_data(),
            "disclosedContracts": disclosed(),
        })))
        .mount(server)
        .await;
    stub_prepare_and_execute(server, prepare_response).await;
}

fn accept_transaction(contract_id: &str, receiver: &str) -> PreparedTransaction {
    prepared(
        exercise(
            contract_id,
            "TransferInstruction",
            ACCEPT_CHOICE,
            &[receiver],
            record(vec![("extraArgs", extra_args())]),
        ),
        receiver,
    )
}

#[tokio::test]
async fn accept_transfer_exercises_the_instruction_with_the_registry_context() {
    let key = ExternalKeyPair::from_seed([27u8; 32]);
    let receiver = key.party_id("bob");
    let transaction = accept_transaction(INSTRUCTION_CID, &receiver);
    let hash = hash_of(&transaction);
    let response = prepare_response(&transaction, &hash);
    let server = MockServer::start().await;
    stub_accept(&server, response.clone()).await;

    let receipt = match accept_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &receiver,
        INSTRUCTION_CID,
    )
    .await
    {
        Ok(receipt) => receipt,
        Err(e) => panic!("the accept must succeed: {e}"),
    };

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
        response["preparedTransaction"]
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
    let transaction = accept_transaction("00b9", &receiver);
    let server = MockServer::start().await;
    stub_accept(
        &server,
        prepare_response(&transaction, &hash_of(&transaction)),
    )
    .await;

    let result = accept_transfer(
        &ledger_for(&server),
        &registry_for(&server),
        &key,
        &receiver,
        INSTRUCTION_CID,
    )
    .await;

    match result {
        Err(Error::TransactionRefused {
            reason: Refusal::Mismatch { field, found, .. },
            ..
        }) => {
            assert_eq!(field, "contract id");
            assert_eq!(found, "00b9");
        }
        Err(other) => panic!("expected a contract id mismatch, got {other}"),
        Ok(_) => panic!("an accept of another instruction must not be signed"),
    }
    assert_eq!(count(&server, EXECUTE_PATH).await, 0, "nothing is executed");
}
