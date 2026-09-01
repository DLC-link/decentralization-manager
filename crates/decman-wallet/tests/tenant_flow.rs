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
    api::{TenantOnboardRequest, TenantPrepareRequest},
    canton_id::CantonId,
};
use decman_wallet::{
    ExternalKeyPair, HostStatus, TenantClient, WalletHost, onboard_co_validated, statuses,
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
async fn an_api_error_carries_the_host_and_the_server_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/tenant/alice::1220ab/status"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": "invalid tenant API key"})),
        )
        .mount(&server)
        .await;

    let client = match TenantClient::new(server.uri(), "wrong-key") {
        Ok(c) => c,
        Err(e) => panic!("client must build: {e}"),
    };
    let error = match client.host_status("alice::1220ab").await {
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

// ============================================================================
// Adding hosts to a party that already exists
// ============================================================================

/// A stub host that prepares an add-hosts topology.
async fn stub_add_hosts_prepare(server: &MockServer, serial: u32) {
    Mock::given(method("POST"))
        .and(path("/v0/tenant/add-hosts/prepare"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "party_id": "alice::1220aa",
            "serial": serial,
            "transaction_hashes": TX_HASHES.map(|h| STANDARD.encode(h)),
            "topology_transactions": ["dHgtb25l", "dHgtdHdv"],
        })))
        .mount(server)
        .await;
}

async fn stub_add_hosts_onboard(server: &MockServer, serial: u32) {
    Mock::given(method("POST"))
        .and(path("/v0/tenant/add-hosts/onboard"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "status": "completed",
            "party_id": "alice::1220aa",
            "serial": serial,
        })))
        .mount(server)
        .await;
}

/// A stub source host that serves the party's ACS as ranges, and a joiner that
/// accepts them and reports completion.
async fn stub_acs_relay(source: &MockServer, joiner: &MockServer) {
    let snapshot = b"an-acs-snapshot".to_vec();
    let total = snapshot.len() as u64;

    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(
            r"^/v0/tenant/.+/acs-progress$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "party_id": "alice::1220aa",
            "received": 0,
        })))
        .mount(joiner)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/v0/tenant/.+/acs/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "party_id": "alice::1220aa",
            "total_size": total,
            "offset": 0,
            "chunk": STANDARD.encode(&snapshot),
            "package_ids": ["pkg-one"],
            "package_preflight": true,
        })))
        .mount(source)
        .await;
    Mock::given(method("POST"))
        .and(path("/v0/tenant/add-hosts/import"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "party_id": "alice::1220aa",
            "received": total,
            "complete": true,
            "imported": true,
            "marker_cleared": true,
        })))
        .mount(joiner)
        .await;
}

/// The safeguard: every host — current and joining — must prepare, because the
/// wallet cannot check one host's word for what it is signing.
#[tokio::test]
async fn add_hosts_prepares_on_every_host_and_submits_only_to_joiners() {
    let (p1, p2, p3) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    for (s, serial) in [(&p1, 5u32), (&p2, 5), (&p3, 5)] {
        stub_add_hosts_prepare(s, serial).await;
    }
    stub_add_hosts_onboard(&p3, 5).await;
    stub_acs_relay(&p1, &p3).await;

    let key = ExternalKeyPair::from_seed([4u8; 32]);
    let current = vec![host_for(&p1, 1), host_for(&p2, 2)];
    let joining = vec![host_for(&p3, 3)];

    let Ok(added) = decman_wallet::add_hosts(&current, &joining, &key, "alice::1220aa", 4).await
    else {
        panic!("a consistent add-hosts must succeed");
    };

    // All three prepared, including the joiner: it reads the mapping from the
    // shared synchronizer store, so it can check the others' work.
    for server in [&p1, &p2, &p3] {
        assert_eq!(
            add_hosts_prepare_calls(server).await,
            1,
            "every host must be asked to prepare"
        );
    }
    // Only the joiner submits: Canton needs the party namespace plus each new
    // participant, and the existing hosts are neither.
    assert_eq!(add_hosts_onboard_calls(&p1).await, 0);
    assert_eq!(add_hosts_onboard_calls(&p2).await, 0);
    assert_eq!(add_hosts_onboard_calls(&p3).await, 1);
    assert!(added.replicated, "the joiner should have been switched on");
    assert!(added.without_package_preflight.is_empty());
}

/// A host that prepares different bytes must stop the run before anything is
/// signed. This is the whole reason every host prepares.
#[tokio::test]
async fn add_hosts_refuses_when_hosts_disagree() {
    let (p1, p2, p3) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    stub_add_hosts_prepare(&p1, 5).await;
    stub_add_hosts_prepare(&p2, 5).await;
    // A different serial is a different transaction.
    stub_add_hosts_prepare(&p3, 9).await;
    stub_add_hosts_onboard(&p3, 5).await;

    let key = ExternalKeyPair::from_seed([4u8; 32]);
    let current = vec![host_for(&p1, 1), host_for(&p2, 2)];
    let joining = vec![host_for(&p3, 3)];

    let Err(e) = decman_wallet::add_hosts(&current, &joining, &key, "alice::1220aa", 4).await
    else {
        panic!("disagreeing hosts must abort the run");
    };
    assert!(format!("{e}").contains("disagree") || format!("{e:?}").contains("HostDisagreement"));
    // Nothing was submitted, so nothing was signed into topology.
    assert_eq!(add_hosts_onboard_calls(&p3).await, 0);
}

/// A threshold change needs fewer Canton signatures than an add, but the wallet
/// still cannot verify one host's hash alone — so one host is refused.
#[tokio::test]
async fn raise_threshold_refuses_a_single_host() {
    let p1 = MockServer::start().await;
    let key = ExternalKeyPair::from_seed([4u8; 32]);

    let Err(e) =
        decman_wallet::raise_threshold(&[host_for(&p1, 1)], &key, "alice::1220aa", 2, 5).await
    else {
        panic!("a lone preparer must be refused");
    };
    assert!(format!("{e}").contains("host"), "{e}");
    // It refused before asking, so no signature was ever produced.
    assert_eq!(threshold_prepare_calls(&p1).await, 0);
}

/// A preparer that returns more hashes than transactions is trying to get a
/// signature over something the wallet was never shown.
#[tokio::test]
async fn raise_threshold_refuses_extra_hashes() {
    let (p1, p2) = (MockServer::start().await, MockServer::start().await);
    for s in [&p1, &p2] {
        Mock::given(method("POST"))
            .and(path("/v0/tenant/threshold/prepare"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "party_id": "alice::1220aa",
                "serial": 6,
                // Three hashes for two transactions.
                "transaction_hashes": [
                    STANDARD.encode(TX_HASHES[0]),
                    STANDARD.encode(TX_HASHES[1]),
                    STANDARD.encode(b"hash-of-something-else"),
                ],
                "topology_transactions": ["dHgtb25l", "dHgtdHdv"],
            })))
            .mount(s)
            .await;
    }

    let key = ExternalKeyPair::from_seed([4u8; 32]);
    let Err(e) = decman_wallet::raise_threshold(
        &[host_for(&p1, 1), host_for(&p2, 2)],
        &key,
        "alice::1220aa",
        2,
        5,
    )
    .await
    else {
        panic!("a hash without a matching transaction must be refused");
    };
    assert!(format!("{e:?}").contains("MalformedPreparation"), "{e}");
}

async fn add_hosts_prepare_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .map(|reqs| {
            reqs.iter()
                .filter(|r| r.url.path() == "/v0/tenant/add-hosts/prepare")
                .count()
        })
        .unwrap_or(0)
}

async fn add_hosts_onboard_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .map(|reqs| {
            reqs.iter()
                .filter(|r| r.url.path() == "/v0/tenant/add-hosts/onboard")
                .count()
        })
        .unwrap_or(0)
}

async fn threshold_prepare_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .map(|reqs| {
            reqs.iter()
                .filter(|r| r.url.path() == "/v0/tenant/threshold/prepare")
                .count()
        })
        .unwrap_or(0)
}
