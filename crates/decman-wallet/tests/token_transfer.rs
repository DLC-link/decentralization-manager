//! Wire-contract tests for Token Standard transfers, against a stub JSON
//! Ledger API and a stub registry.
//!
//! One stub server plays both, under different path prefixes, so a test can
//! read the order of every call the wallet made.

use decman_wallet::{
    Error, LedgerClient, RegistryClient, TransferArgs,
    transfer::{HOLDING_INTERFACE_ID, TRANSFER_INSTRUCTION_INTERFACE_ID},
};
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
