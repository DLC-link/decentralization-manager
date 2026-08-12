//! Wire-contract tests for the wallet-side tenant client, against stub hosts.
//!
//! What these pin down is the part a wallet provider has to trust: every host
//! independently builds the topology and must agree byte-for-byte, the wallet signs
//! it locally, and the *same* signed bundle reaches every host — with a signature
//! that verifies under the public key the wallet published, and no private key
//! anywhere on the wire.
//!
//! The agreement check is load-bearing rather than decorative: the wallet signs
//! hashes it does not compute, so a single host's word for what it is signing is not
//! something it can verify alone.

use base64::{Engine, engine::general_purpose::STANDARD};
use common::{
    api::{TenantExecuteSubmissionRequest, TenantOnboardRequest, TenantPrepareRequest},
    canton_id::CantonId,
};
use decman_wallet::{
    ExternalKeyPair, HostStatus, TenantClient, WalletHost, create_contract, onboard_co_validated,
    statuses,
};
use ed25519_dalek::{Verifier, VerifyingKey};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// A deterministic, valid `participant::1220…` id (2-byte multihash prefix + 32
/// bytes) so party ids in these tests are stable.
fn participant_id(tag: u8) -> CantonId {
    let namespace = format!("1220{}", format!("{tag:02x}").repeat(32));
    match CantonId::parse(&format!("participant::{namespace}")) {
        Ok(id) => id,
        Err(e) => panic!("test participant id must parse: {e}"),
    }
}

/// The per-transaction hashes a stub host hands back for signing. Two entries, so
/// the tests also cover that each signature lands on its own transaction.
const TX_HASHES: [&[u8]; 2] = [b"canton-hash-for-tx-one", b"canton-hash-for-tx-two"];

async fn stub_prepare(server: &MockServer, party_id: &str) {
    Mock::given(method("POST"))
        .and(path("/v0/tenant/prepare"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "party_id": party_id,
            "transaction_hashes": TX_HASHES.map(|h| STANDARD.encode(h)),
            "topology_transactions": ["dHgtb25l", "dHgtdHdv"],
        })))
        .mount(server)
        .await;
}

async fn stub_onboard(server: &MockServer, party_id: &str, status: &str) {
    Mock::given(method("POST"))
        .and(path("/v0/tenant/onboard"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "status": status,
            "party_id": party_id,
        })))
        .mount(server)
        .await;
}

fn host_for(server: &MockServer, tag: u8) -> WalletHost {
    let client = match TenantClient::new(server.uri(), "test-tenant-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };
    WalletHost::new(client, participant_id(tag))
}

/// How many topology-prepare calls this stub host received.
async fn prepare_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/v0/tenant/prepare")
        .count()
}

/// Every onboard body the stub host received, decoded.
async fn onboard_bodies(server: &MockServer) -> Vec<TenantOnboardRequest> {
    let requests = server.received_requests().await.unwrap_or_default();
    requests
        .iter()
        .filter(|r| r.url.path() == "/v0/tenant/onboard")
        .map(
            |r| match serde_json::from_slice::<TenantOnboardRequest>(&r.body) {
                Ok(body) => body,
                Err(e) => panic!("onboard body must match the wire DTO: {e}"),
            },
        )
        .collect()
}

#[tokio::test]
async fn onboarding_has_every_host_prepare_and_submits_one_signed_bundle_to_all() {
    let key = ExternalKeyPair::from_seed([7u8; 32]);
    let party_id = key.party_id("alice");

    let (p1, p2, p3) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    for server in [&p1, &p2, &p3] {
        stub_prepare(server, &party_id).await;
    }
    for (server, status) in [(&p1, "inprogress"), (&p2, "inprogress"), (&p3, "completed")] {
        stub_onboard(server, &party_id, status).await;
    }
    let hosts = vec![
        host_for(&p1, 0x11),
        host_for(&p2, 0x22),
        host_for(&p3, 0x33),
    ];

    let party = match onboard_co_validated(&hosts, &key, "alice", Some(2)).await {
        Ok(p) => p,
        Err(e) => panic!("onboarding must succeed against stub hosts: {e}"),
    };

    assert_eq!(party.party_id, party_id);
    assert_eq!(party.fingerprint, key.fingerprint());
    assert_eq!(party.public_key, key.public_key_b64());
    assert_eq!(party.hosts.len(), 3);

    // Every host is asked to build the topology, exactly once each: their answers
    // are what the wallet checks against each other before it signs.
    for server in [&p1, &p2, &p3] {
        assert_eq!(prepare_calls(server).await, 1);
    }

    // The prepare names the other two hosts as hosting peers, and carries only
    // the public key.
    let prepare_body = {
        let requests = p1.received_requests().await.unwrap_or_default();
        let raw = match requests
            .iter()
            .find(|r| r.url.path() == "/v0/tenant/prepare")
        {
            Some(r) => r.body.clone(),
            None => panic!("the first host must have received a prepare"),
        };
        match serde_json::from_slice::<TenantPrepareRequest>(&raw) {
            Ok(body) => body,
            Err(e) => panic!("prepare body must match the wire DTO: {e}"),
        }
    };
    assert_eq!(prepare_body.party_hint, "alice");
    assert_eq!(prepare_body.public_key, key.public_key_b64());
    assert_eq!(prepare_body.confirmation_threshold, Some(2));
    assert_eq!(
        prepare_body.hosting_peers,
        vec![participant_id(0x22), participant_id(0x33)],
        "the peers are every host except the one that prepared"
    );

    // Each host got exactly one onboard, and all three bundles are identical —
    // the wallet fans out one signed artifact rather than re-signing per host.
    let mut bundles = Vec::new();
    for server in [&p1, &p2, &p3] {
        let bodies = onboard_bodies(server).await;
        assert_eq!(bodies.len(), 1, "each host is onboarded exactly once");
        match bodies.into_iter().next() {
            Some(bundle) => bundles.push(bundle),
            None => panic!("length was just asserted"),
        }
    }
    for bundle in &bundles[1..] {
        assert_eq!(bundle.signatures, bundles[0].signatures);
        assert_eq!(
            bundle.topology_transactions,
            bundles[0].topology_transactions
        );
        assert_eq!(bundle.public_key, bundles[0].public_key);
        assert_eq!(bundle.signed_by, bundles[0].signed_by);
    }

    // Each signature verifies over its own transaction's hash, under the
    // public key the wallet published — and `signed_by` is that key's fingerprint.
    let bundle = &bundles[0];
    assert_eq!(bundle.signed_by, key.fingerprint());
    assert_eq!(
        bundle.topology_transactions,
        vec!["dHgtb25l".to_string(), "dHgtdHdv".to_string()],
        "topology transactions are relayed back unchanged"
    );
    let public_key: [u8; 32] = match STANDARD.decode(&bundle.public_key).map(TryInto::try_into) {
        Ok(Ok(bytes)) => bytes,
        _ => panic!("public_key on the wire must be 32 base64-encoded bytes"),
    };
    let verifying = match VerifyingKey::from_bytes(&public_key) {
        Ok(v) => v,
        Err(e) => panic!("public_key must be a valid Ed25519 key: {e}"),
    };
    assert_eq!(
        bundle.signatures.len(),
        TX_HASHES.len(),
        "one signature per transaction"
    );
    for (i, encoded) in bundle.signatures.iter().enumerate() {
        let signature = match STANDARD.decode(encoded).map(TryInto::try_into) {
            Ok(Ok(bytes)) => ed25519_dalek::Signature::from_bytes(&bytes),
            _ => panic!("signature {i} must be 64 base64-encoded bytes"),
        };
        assert!(
            verifying.verify(TX_HASHES[i], &signature).is_ok(),
            "signature {i} must verify over transaction {i}'s own hash, under the published key"
        );
        // And must NOT verify against a different transaction's hash — proves the
        // signatures are per-transaction rather than one hash reused.
        let other = TX_HASHES[(i + 1) % TX_HASHES.len()];
        assert!(
            verifying.verify(other, &signature).is_err(),
            "signature {i} must not authorize a different transaction"
        );
    }

    // Nothing on the wire may carry the private seed.
    let seed_b64 = key.seed_b64();
    for server in [&p1, &p2, &p3] {
        for request in server.received_requests().await.unwrap_or_default() {
            let body = String::from_utf8_lossy(&request.body);
            assert!(
                !body.contains(seed_b64.as_str()),
                "the private seed must never appear in a request body"
            );
        }
    }
}

#[tokio::test]
async fn onboarding_refuses_to_sign_when_the_host_derives_a_different_party_id() {
    let key = ExternalKeyPair::from_seed([8u8; 32]);

    let (p1, p2) = (MockServer::start().await, MockServer::start().await);
    // The host answers with a party id that does not match this key's fingerprint.
    stub_prepare(&p1, "alice::1220deadbeef").await;
    stub_prepare(&p2, "alice::1220deadbeef").await;
    stub_onboard(&p1, "alice::1220deadbeef", "inprogress").await;
    stub_onboard(&p2, "alice::1220deadbeef", "inprogress").await;
    let hosts = vec![host_for(&p1, 0x11), host_for(&p2, 0x22)];

    let error = match onboard_co_validated(&hosts, &key, "alice", None).await {
        Ok(_) => panic!("a party-id mismatch must not be onboarded"),
        Err(e) => e,
    };
    assert!(
        error.to_string().contains("refusing to sign"),
        "unexpected error: {error}"
    );

    // Crucially: it bailed out *before* signing and fanning out.
    for server in [&p1, &p2] {
        assert!(
            onboard_bodies(server).await.is_empty(),
            "no bundle may be submitted after a party-id mismatch"
        );
    }
}

/// The attack this defends against: the wallet signs hashes it does not compute, so
/// a preparing host could hand it plausible transaction bytes together with the hash
/// of a *different* mapping — one adding that host's own key to `party_signing_keys`
/// — and the wallet's signature would authorize it. Agreement between hosts is the
/// only thing that catches it, so a single dissenting host must stop the run before
/// anything is signed.
#[tokio::test]
async fn refuses_to_sign_when_one_host_returns_a_different_hash() {
    let key = ExternalKeyPair::from_seed([11u8; 32]);
    let party_id = key.party_id("alice");

    let (p1, p2, p3) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    stub_prepare(&p1, &party_id).await;
    stub_prepare(&p3, &party_id).await;
    // Same party id, same transaction bytes, different hash to sign.
    Mock::given(method("POST"))
        .and(path("/v0/tenant/prepare"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "party_id": party_id,
            "transaction_hashes": [STANDARD.encode(b"hash-of-a-mapping-you-never-saw"), STANDARD.encode(TX_HASHES[1])],
            "topology_transactions": ["dHgtb25l", "dHgtdHdv"],
        })))
        .mount(&p2)
        .await;
    for server in [&p1, &p2, &p3] {
        stub_onboard(server, &party_id, "completed").await;
    }
    let hosts = vec![
        host_for(&p1, 0x11),
        host_for(&p2, 0x22),
        host_for(&p3, 0x33),
    ];

    let error = match onboard_co_validated(&hosts, &key, "alice", Some(2)).await {
        Ok(_) => panic!("a host that disagrees about the hash must stop onboarding"),
        Err(e) => e,
    };
    assert!(
        error.to_string().contains("different hashes to sign"),
        "the error should name what diverged: {error}"
    );
    assert_eq!(
        error.host(),
        p2.uri().trim_end_matches('/'),
        "the dissenting host must be named"
    );

    for server in [&p1, &p2, &p3] {
        assert!(
            onboard_bodies(server).await.is_empty(),
            "nothing may be signed or submitted once the hosts disagree"
        );
    }
}

/// Divergent transaction bytes are the same failure from the other direction.
#[tokio::test]
async fn refuses_to_sign_when_one_host_returns_different_transactions() {
    let key = ExternalKeyPair::from_seed([12u8; 32]);
    let party_id = key.party_id("alice");

    let (p1, p2) = (MockServer::start().await, MockServer::start().await);
    stub_prepare(&p1, &party_id).await;
    Mock::given(method("POST"))
        .and(path("/v0/tenant/prepare"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "party_id": party_id,
            "transaction_hashes": TX_HASHES.map(|h| STANDARD.encode(h)),
            "topology_transactions": ["c29tZXRoaW5nLWVsc2U=", "dHgtdHdv"],
        })))
        .mount(&p2)
        .await;
    let hosts = vec![host_for(&p1, 0x11), host_for(&p2, 0x22)];

    let error = match onboard_co_validated(&hosts, &key, "alice", None).await {
        Ok(_) => panic!("divergent topology must stop onboarding"),
        Err(e) => e,
    };
    assert!(
        error
            .to_string()
            .contains("different topology transactions"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn a_failing_host_is_reported_without_stopping_the_others() {
    let key = ExternalKeyPair::from_seed([9u8; 32]);
    let party_id = key.party_id("alice");

    let (p1, p2, p3) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    for server in [&p1, &p2, &p3] {
        stub_prepare(server, &party_id).await;
    }
    stub_onboard(&p1, &party_id, "completed").await;
    Mock::given(method("POST"))
        .and(path("/v0/tenant/onboard"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": "allocate failed here"})),
        )
        .mount(&p2)
        .await;
    stub_onboard(&p3, &party_id, "completed").await;
    let hosts = vec![
        host_for(&p1, 0x11),
        host_for(&p2, 0x22),
        host_for(&p3, 0x33),
    ];

    let party = match onboard_co_validated(&hosts, &key, "alice", None).await {
        Ok(p) => p,
        Err(e) => panic!("one bad host must not fail the whole run: {e}"),
    };

    assert!(party.hosts[0].is_hosted());
    assert!(!party.hosts[1].is_hosted());
    assert_eq!(party.hosts[1].status, None);
    let reported = party.hosts[1].error.clone().unwrap_or_default();
    assert!(
        reported.contains("allocate failed here"),
        "the host's own message should survive: {reported}"
    );
    assert!(
        party.hosts[2].is_hosted(),
        "the host after the failure must still be attempted"
    );
    assert!(!party.fully_hosted());
}

#[tokio::test]
async fn onboarding_needs_more_than_one_host() {
    let key = ExternalKeyPair::from_seed([1u8; 32]);
    let p1 = MockServer::start().await;
    let hosts = vec![host_for(&p1, 0x11)];

    let error = match onboard_co_validated(&hosts, &key, "alice", None).await {
        Ok(_) => panic!("a single host is not co-validation"),
        Err(e) => e,
    };
    assert!(error.to_string().contains("at least two hosts"));
    assert!(
        p1.received_requests().await.unwrap_or_default().is_empty(),
        "the guard must fire before any host is contacted"
    );
}

#[tokio::test]
async fn status_maps_each_host_answer_including_a_404() {
    let key = ExternalKeyPair::from_seed([3u8; 32]);
    let party_id = key.party_id("alice");

    let (hosted, pending, absent) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    for (server, status) in [(&hosted, "completed"), (&pending, "inprogress")] {
        Mock::given(method("GET"))
            .and(path(format!("/v0/tenant/{party_id}/status")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": status})))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/v0/tenant/{party_id}/status")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not hosted here"})))
        .mount(&absent)
        .await;

    let hosts = vec![
        host_for(&hosted, 0x11),
        host_for(&pending, 0x22),
        host_for(&absent, 0x33),
    ];
    let reports = statuses(&hosts, &party_id).await;

    assert_eq!(reports[0].status, Some(HostStatus::Hosted));
    assert_eq!(reports[1].status, Some(HostStatus::Pending));
    assert_eq!(
        reports[2].status,
        Some(HostStatus::NotHosted),
        "a 404 is a host saying 'not mine', not a transport failure"
    );
    assert!(reports.iter().all(|r| r.error.is_none()));
}

#[tokio::test]
async fn creating_a_contract_signs_the_prepared_transaction_hash() {
    let key = ExternalKeyPair::from_seed([5u8; 32]);
    let party_id = key.party_id("alice");
    let hash = b"prepared-transaction-hash";

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/tenant/{party_id}/prepare-submission")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "prepared_transaction": "cHJlcGFyZWQ=",
            "prepared_transaction_hash": STANDARD.encode(hash),
            "hashing_scheme_version": 2,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/tenant/{party_id}/execute-submission")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = match TenantClient::new(server.uri(), "test-tenant-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };
    let template_id = common::api::TenantTemplateId {
        package_id: "pkg".to_string(),
        module_name: "Mod".to_string(),
        entity_name: "Ent".to_string(),
    };
    if let Err(e) = create_contract(
        &client,
        &key,
        &party_id,
        template_id,
        json!({"owner": "alice", "amount": 5}),
    )
    .await
    {
        panic!("create must succeed against stub hosts: {e}");
    }

    let requests = server.received_requests().await.unwrap_or_default();
    let raw = match requests
        .iter()
        .find(|r| r.url.path().ends_with("/execute-submission"))
    {
        Some(r) => r.body.clone(),
        None => panic!("execute-submission must be called"),
    };
    let body = match serde_json::from_slice::<TenantExecuteSubmissionRequest>(&raw) {
        Ok(b) => b,
        Err(e) => panic!("execute body must match the wire DTO: {e}"),
    };

    assert_eq!(
        body.prepared_transaction, "cHJlcGFyZWQ=",
        "the prepared transaction is relayed back unchanged"
    );
    assert_eq!(body.signed_by, key.fingerprint());
    assert_eq!(
        body.hashing_scheme_version, 2,
        "the scheme version must be echoed from prepare, not assumed"
    );

    let verifying = match VerifyingKey::from_bytes(&key.public_key_bytes()) {
        Ok(v) => v,
        Err(e) => panic!("key must be valid: {e}"),
    };
    let signature = match STANDARD.decode(&body.signature).map(TryInto::try_into) {
        Ok(Ok(bytes)) => ed25519_dalek::Signature::from_bytes(&bytes),
        _ => panic!("signature must be 64 base64-encoded bytes"),
    };
    assert!(
        verifying.verify(hash, &signature).is_ok(),
        "the signature must be over the prepared-transaction hash"
    );
}

/// Stub one host's prepare-and-execute pair for `party_id`, where `prepare_path`
/// is the endpoint that builds the transaction. Returns the hash the stub asks the
/// wallet to sign.
async fn stub_signing_round(
    server: &MockServer,
    party_id: &str,
    prepare_path: &str,
) -> &'static [u8] {
    let hash: &[u8] = b"prepared-transaction-hash";
    Mock::given(method("POST"))
        .and(path(format!("/v0/tenant/{party_id}/{prepare_path}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "prepared_transaction": "cHJlcGFyZWQ=",
            "prepared_transaction_hash": STANDARD.encode(hash),
            "hashing_scheme_version": 2,
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/tenant/{party_id}/execute-submission")))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
    hash
}

/// The signature the wallet sent to `server`, verified against `key` over `hash`.
/// Panics unless exactly that is what crossed the wire — the whole security claim
/// of the tenant API is that only a signature does.
async fn assert_signed_over(server: &MockServer, key: &ExternalKeyPair, hash: &[u8]) {
    let requests = server.received_requests().await.unwrap_or_default();
    let raw = match requests
        .iter()
        .find(|r| r.url.path().ends_with("/execute-submission"))
    {
        Some(r) => r.body.clone(),
        None => panic!("execute-submission must be called"),
    };
    let body = match serde_json::from_slice::<TenantExecuteSubmissionRequest>(&raw) {
        Ok(b) => b,
        Err(e) => panic!("execute body must match the wire DTO: {e}"),
    };
    assert_eq!(body.signed_by, key.fingerprint());

    let verifying = match VerifyingKey::from_bytes(&key.public_key_bytes()) {
        Ok(v) => v,
        Err(e) => panic!("key must be valid: {e}"),
    };
    let signature = match STANDARD.decode(&body.signature).map(TryInto::try_into) {
        Ok(Ok(bytes)) => ed25519_dalek::Signature::from_bytes(&bytes),
        _ => panic!("signature must be 64 base64-encoded bytes"),
    };
    assert!(
        verifying.verify(hash, &signature).is_ok(),
        "the signature must be over the prepared-transaction hash"
    );
}

/// The request body the wallet posted to `path_suffix`, as JSON.
async fn posted_body(server: &MockServer, path_suffix: &str) -> serde_json::Value {
    let requests = server.received_requests().await.unwrap_or_default();
    let raw = match requests
        .iter()
        .find(|r| r.url.path().ends_with(path_suffix))
    {
        Some(r) => r.body.clone(),
        None => panic!("{path_suffix} must be called"),
    };
    match serde_json::from_slice(&raw) {
        Ok(body) => body,
        Err(e) => panic!("{path_suffix} body must be JSON: {e}"),
    }
}

/// A create command must reach the host tagged as one, now that a submission can
/// also carry an exercise.
#[tokio::test]
async fn a_create_is_tagged_as_a_create_on_the_wire() {
    let key = ExternalKeyPair::from_seed([9u8; 32]);
    let party_id = key.party_id("alice");

    let server = MockServer::start().await;
    stub_signing_round(&server, &party_id, "prepare-submission").await;

    let template_id = common::api::TenantTemplateId {
        package_id: "pkg".to_string(),
        module_name: "Mod".to_string(),
        entity_name: "Ent".to_string(),
    };
    let client = match TenantClient::new(server.uri(), "test-tenant-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };
    if let Err(e) = create_contract(
        &client,
        &key,
        &party_id,
        template_id,
        json!({"owner": "alice"}),
    )
    .await
    {
        panic!("create must succeed against stub hosts: {e}");
    }

    let body = posted_body(&server, "/prepare-submission").await;
    assert_eq!(
        body["command"]["create"]["template_id"]["entity_name"], "Ent",
        "the create command must be tagged `create`: {body}"
    );
    assert!(
        body["command"].get("exercise").is_none(),
        "a create must not also look like an exercise: {body}"
    );
}

/// Sending is the same signing round as anything else: the host resolves the
/// registry and prepares, and only a signature goes back.
#[tokio::test]
async fn sending_an_asset_signs_what_the_host_prepared() {
    let key = ExternalKeyPair::from_seed([6u8; 32]);
    let party_id = key.party_id("alice");

    let server = MockServer::start().await;
    let hash = stub_signing_round(&server, &party_id, "prepare-transfer").await;

    let client = match TenantClient::new(server.uri(), "test-tenant-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };
    let request = common::api::TenantTransferRequest {
        receiver: "bob::1220bb".to_string(),
        instrument_admin: "DSO::1220dd".to_string(),
        instrument_id: "Amulet".to_string(),
        amount: "1.25".to_string(),
        input_holding_cids: Vec::new(),
        validity_window_hours: None,
    };
    if let Err(e) = decman_wallet::send_transfer(&client, &key, &party_id, &request).await {
        panic!("send must succeed against stub hosts: {e}");
    }

    let body = posted_body(&server, "/prepare-transfer").await;
    assert_eq!(body["receiver"], "bob::1220bb");
    assert_eq!(
        body["amount"], "1.25",
        "the amount must stay a decimal string, not become a float: {body}"
    );
    assert_signed_over(&server, &key, hash).await;
}

/// Accepting an inbound transfer is the receiver's own signature — the half of the
/// flow that a wallet-held key has to be able to do unaided.
#[tokio::test]
async fn accepting_an_offer_signs_what_the_host_prepared() {
    let key = ExternalKeyPair::from_seed([7u8; 32]);
    let party_id = key.party_id("alice");

    let server = MockServer::start().await;
    let hash = stub_signing_round(&server, &party_id, "prepare-accept").await;

    let client = match TenantClient::new(server.uri(), "test-tenant-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };
    if let Err(e) = decman_wallet::accept_transfer(&client, &key, &party_id, "00instruction").await
    {
        panic!("accept must succeed against stub hosts: {e}");
    }

    let body = posted_body(&server, "/prepare-accept").await;
    assert_eq!(body["transfer_instruction_cid"], "00instruction");
    assert_signed_over(&server, &key, hash).await;
}

/// Balances and inbound offers are plain reads, but their amounts must survive as
/// strings — a JSON number would round a Daml `Decimal`'s ten fractional digits.
#[tokio::test]
async fn balances_and_offers_decode_from_a_host() {
    let party_id = "alice::1220ab";
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/tenant/{party_id}/holdings")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "holdings": [{
                "instrument_admin": "DSO::1220dd",
                "instrument_id": "Amulet",
                "total": "10.0000000001",
                "locked": "2.5",
                "available": "7.5000000001",
            }],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/tenant/{party_id}/transfer-offers")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offers": [{
                "contract_id": "00instruction",
                "sender": "bob::1220bb",
                "instrument_admin": "DSO::1220dd",
                "instrument_id": "Amulet",
                "amount": "3.0",
                "acceptable": true,
                "expires_at": 1_700_000_000,
            }],
        })))
        .mount(&server)
        .await;

    let client = match TenantClient::new(server.uri(), "test-tenant-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };

    let holdings = match client.holdings(party_id).await {
        Ok(h) => h,
        Err(e) => panic!("holdings must decode: {e}"),
    };
    assert_eq!(holdings.len(), 1);
    assert_eq!(
        holdings[0].available, "7.5000000001",
        "the tenth fractional digit must survive the round trip"
    );

    let offers = match client.transfer_offers(party_id).await {
        Ok(o) => o,
        Err(e) => panic!("offers must decode: {e}"),
    };
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].contract_id, "00instruction");
    assert!(offers[0].acceptable);
}

#[tokio::test]
async fn an_api_error_carries_the_host_and_the_server_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/tenant/alice::1220ab/acs"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": "invalid tenant API key"})),
        )
        .mount(&server)
        .await;

    let client = match TenantClient::new(server.uri(), "wrong-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };
    let error = match client.acs("alice::1220ab").await {
        Ok(_) => panic!("a 401 must not read as success"),
        Err(e) => e,
    };

    assert!(error.is_status(401));
    assert_eq!(error.host(), server.uri().trim_end_matches('/'));
    assert!(
        error.to_string().contains("invalid tenant API key"),
        "the server's message should reach the operator: {error}"
    );
}
