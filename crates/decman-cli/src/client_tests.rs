//! The API client and the commands against a fake IdP and a fake decman node.

use clap::Parser;
use reqwest::Method;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::api::{AuthSettings, DecmanClient};
use crate::auth::{AuthTarget, PasswordSource, TokenCache, TokenSet};
use crate::cli::Cli;
use crate::command::{self, Command};

/// A fake server on its own runtime. The blocking client must not run inside
/// a runtime, so the tests call it from the test thread.
struct Fake {
    // Declared first so it drops, and checks its expectations, before the
    // runtime it runs on.
    server: MockServer,
    runtime: tokio::runtime::Runtime,
}

impl Fake {
    fn start() -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        Self { server, runtime }
    }

    fn mount(&self, mock: Mock) {
        self.runtime.block_on(mock.mount(&self.server));
    }

    fn uri(&self) -> String {
        self.server.uri()
    }

    fn auth(&self) -> AuthSettings {
        AuthSettings {
            username: "alice".to_owned(),
            password: Some(PasswordSource::Literal("pw".to_owned())),
            token_url: Some(format!("{}/token", self.uri())),
            client_id: Some("cli".to_owned()),
            client_secret: None,
            audience: None,
            scope: None,
        }
    }

    fn target(&self) -> AuthTarget {
        AuthTarget {
            token_url: format!("{}/token", self.uri()),
            client_id: "cli".to_owned(),
            audience: None,
        }
    }

    /// The token endpoint answers `grant` with access token `access`.
    fn token_grant(&self, grant: &str, access: &str, calls: u64) {
        self.mount(
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains(format!("grant_type={grant}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "access_token": access,
                    "expires_in": 300,
                    "refresh_token": "r-new",
                    "refresh_expires_in": 1800,
                })))
                .expect(calls),
        );
    }

    /// `GET route` answers `body` to a request with bearer `access`.
    fn get_with_token(&self, route: &str, access: &str, body: Value, calls: u64) {
        self.mount(
            Mock::given(method("GET"))
                .and(path(route))
                .and(header("authorization", format!("Bearer {access}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(calls),
        );
    }

    fn client(&self) -> DecmanClient {
        DecmanClient::new(self.uri(), self.auth()).unwrap()
    }
}

fn tokens(access: &str, access_expires_at: i64, refresh: &str) -> TokenSet {
    TokenSet {
        access_token: access.to_owned(),
        access_expires_at: Some(access_expires_at),
        refresh_token: Some(refresh.to_owned()),
        refresh_expires_at: None,
    }
}

fn parse(args: &[&str]) -> Command {
    let mut argv = vec!["decman-cli"];
    argv.extend_from_slice(args);
    Cli::try_parse_from(argv).unwrap().command.unwrap()
}

fn run_json(status: &str, extra: Value) -> Value {
    let mut run = json!({
        "instance_name": "dars-7",
        "kind": "Dars",
        "role": "Coordinator",
        "status": status,
        "current_step": "Done",
        "step_index": 3,
        "step_total": 3,
        "expected_peers": [],
        "completed_peers": [],
        "dismissed": false,
        "created_at": 1,
        "updated_at": 2,
    });
    if let (Some(run), Value::Object(extra)) = (run.as_object_mut(), extra) {
        run.extend(extra);
    }
    run
}

#[test]
fn a_cached_login_serves_the_next_invocation() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.get_with_token("/node-config", "a-1", json!({ "ok": true }), 2);
    let dir = tempfile::tempdir().unwrap();
    let cache = TokenCache::in_dir(dir.path(), &fake.uri(), "alice");

    // Act — two clients, as two separate CLI invocations.
    let first = fake.client().with_token_cache(cache.clone()).request(
        Method::GET,
        "/node-config",
        &[],
        None,
    );
    let second =
        fake.client()
            .with_token_cache(cache)
            .request(Method::GET, "/node-config", &[], None);

    // Assert — the token endpoint expectation (one call) is checked on drop.
    assert_eq!(first.unwrap(), json!({ "ok": true }));
    assert_eq!(second.unwrap(), json!({ "ok": true }));
}

#[test]
fn an_expired_access_token_is_refreshed_without_the_password() {
    // Arrange
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let cache = TokenCache::in_dir(dir.path(), &fake.uri(), "alice");
    cache
        .save(&fake.target(), &tokens("a-old", 1, "r-old"))
        .unwrap();
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=r-old"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a-2",
                "expires_in": 300,
            })))
            .expect(1),
    );
    fake.token_grant("password", "unused", 0);
    fake.get_with_token("/node-config", "a-2", json!({}), 1);

    // Act
    let result = fake.client().with_token_cache(cache.clone()).request(
        Method::GET,
        "/node-config",
        &[],
        None,
    );

    // Assert — the new access token is cached, and the old refresh token kept.
    result.unwrap();
    let cached = cache.load().unwrap().tokens;
    assert_eq!(cached.access_token, "a-2");
    assert_eq!(cached.refresh_token.as_deref(), Some("r-old"));
}

#[test]
fn a_401_refreshes_once_and_retries() {
    // Arrange
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let cache = TokenCache::in_dir(dir.path(), &fake.uri(), "alice");
    cache
        .save(&fake.target(), &tokens("a-revoked", i64::MAX / 2, "r-old"))
        .unwrap();
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/node-config"))
            .and(header("authorization", "Bearer a-revoked"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1),
    );
    fake.token_grant("refresh_token", "a-2", 1);
    fake.get_with_token("/node-config", "a-2", json!({ "ok": 1 }), 1);

    // Act
    let result =
        fake.client()
            .with_token_cache(cache)
            .request(Method::GET, "/node-config", &[], None);

    // Assert
    assert_eq!(result.unwrap(), json!({ "ok": 1 }));
}

#[test]
fn a_failed_refresh_falls_back_to_the_password() {
    // Arrange
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let cache = TokenCache::in_dir(dir.path(), &fake.uri(), "alice");
    cache
        .save(&fake.target(), &tokens("a-old", 1, "r-expired"))
        .unwrap();
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid_grant"))
            .expect(1),
    );
    fake.token_grant("password", "a-3", 1);
    fake.get_with_token("/node-config", "a-3", json!({}), 1);

    // Act
    let result =
        fake.client()
            .with_token_cache(cache)
            .request(Method::GET, "/node-config", &[], None);

    // Assert
    result.unwrap();
}

#[test]
fn a_login_another_invocation_refreshed_is_used_without_refreshing_again() {
    // Arrange — this client loads an expired login; then a parallel
    // invocation writes a fresh one to the same cache file.
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let cache = TokenCache::in_dir(dir.path(), &fake.uri(), "alice");
    cache
        .save(&fake.target(), &tokens("a-old", 1, "r-old"))
        .unwrap();
    let mut client = fake.client().with_token_cache(cache.clone());
    cache
        .save(&fake.target(), &tokens("a-fresh", i64::MAX / 2, "r-fresh"))
        .unwrap();
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0),
    );
    fake.get_with_token("/node-config", "a-fresh", json!({}), 1);

    // Act
    let result = client.request(Method::GET, "/node-config", &[], None);

    // Assert
    result.unwrap();
}

#[test]
fn a_stale_cached_token_endpoint_falls_back_to_discovery() {
    // Arrange — the cached login names a token endpoint the IdP no longer has.
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let cache = TokenCache::in_dir(dir.path(), &fake.uri(), "alice");
    let stale = AuthTarget {
        token_url: format!("{}/old-token", fake.uri()),
        client_id: "cli".to_owned(),
        audience: None,
    };
    cache.save(&stale, &tokens("a-old", 1, "r-old")).unwrap();
    fake.mount(
        Mock::given(path("/old-token"))
            .respond_with(ResponseTemplate::new(404))
            .expect(2),
    );
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/auth-config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "auth_required": true,
                "keycloak_host": fake.uri(),
                "keycloak_realm": "r",
                "keycloak_client_id": "cli",
            })))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/realms/r/protocol/openid-connect/token"))
            .and(body_string_contains("grant_type=password"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a-new",
                "expires_in": 300,
            })))
            .expect(1),
    );
    fake.get_with_token("/node-config", "a-new", json!({}), 1);
    let mut auth = fake.auth();
    auth.token_url = None;
    auth.client_id = None;

    // Act
    let result = DecmanClient::new(fake.uri(), auth)
        .unwrap()
        .with_token_cache(cache.clone())
        .request(Method::GET, "/node-config", &[], None);

    // Assert — the cache now holds the discovered endpoint.
    result.unwrap();
    let cached = cache.load().unwrap();
    assert!(
        cached
            .target
            .token_url
            .ends_with("/realms/r/protocol/openid-connect/token")
    );
}

#[test]
fn blocked_writes_never_reach_the_server() {
    // Arrange
    let fake = Fake::start();
    fake.mount(
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0),
    );
    let mut client = fake
        .client()
        .with_writes_blocked("profile mainnet needs --yes".to_owned());

    // Act
    let error = client
        .request(Method::POST, "/kick", &[], Some(&json!({})))
        .unwrap_err();

    // Assert
    assert_eq!(error.to_string(), "profile mainnet needs --yes");
}

#[test]
fn an_api_error_carries_status_and_body() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/kick"))
            .respond_with(ResponseTemplate::new(409).set_body_string("busy"))
            .expect(1),
    );

    // Act
    let error = fake
        .client()
        .request(Method::POST, "/kick", &[], Some(&json!({})))
        .unwrap_err();

    // Assert
    let api = error.downcast_ref::<crate::api::ApiError>().unwrap();
    assert_eq!(api.status, reqwest::StatusCode::CONFLICT);
    assert_eq!(api.body, "busy");
}

#[test]
fn distribute_with_wait_follows_the_started_run() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    let dir = tempfile::tempdir().unwrap();
    let dar = dir.path().join("pkg.dar");
    std::fs::write(&dar, b"dar").unwrap();
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/dars/distribute"))
            .and(body_json(json!({
                "dar_files": [{ "filename": "pkg.dar", "data": "ZGFy" }],
                "peer_ids": ["PAR::1220ff"],
            })))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({
                "status": "inprogress",
                "message": "started",
                "instance_name": "dars-7",
            })))
            .expect(1),
    );
    fake.get_with_token(
        "/workflows",
        "a-1",
        json!({ "runs": [run_json("completed", json!({}))] }),
        1,
    );
    let dar_arg = dar.display().to_string();
    let command = parse(&[
        "dars",
        "distribute",
        &dar_arg,
        "--peer",
        "PAR::1220ff",
        "--wait",
    ]);

    // Act
    let outcome = command::run(&mut fake.client(), command).unwrap();

    // Assert
    assert!(outcome.success);
    assert_eq!(outcome.value["instance_name"], "dars-7");
    assert_eq!(outcome.value["status"], "completed");
}

#[test]
fn waiting_on_a_failed_run_is_unsuccessful() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.get_with_token(
        "/workflows",
        "a-1",
        json!({ "runs": [run_json("failed", json!({ "error": "peer gave up" }))] }),
        1,
    );

    // Act
    let outcome =
        command::run(&mut fake.client(), parse(&["workflows", "wait", "dars-7"])).unwrap();

    // Assert
    assert!(!outcome.success);
    assert_eq!(outcome.value["error"], "peer gave up");
}

#[test]
fn accepting_with_wait_follows_this_nodes_peer_run() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.get_with_token(
        "/invitations",
        "a-1",
        json!({ "invitations": [{
            "id": "inv-1",
            "invitation_type": "Dars",
            "coordinator_pubkey": "pk",
            "received_at": 1,
            "workflow_instance": "coord-9",
        }] }),
        1,
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/invitations/accept"))
            .and(body_json(json!({ "id": "inv-1" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "message": "accepted" })),
            )
            .expect(1),
    );
    let peer_run = run_json(
        "completed",
        json!({
            "instance_name": "peer-3",
            "role": "Peer",
            "coordinator_instance": "coord-9",
            "coordinator_pubkey": "pk",
        }),
    );
    fake.get_with_token("/workflows", "a-1", json!({ "runs": [peer_run] }), 1);

    // Act
    let outcome = command::run(
        &mut fake.client(),
        parse(&["invitations", "accept", "inv-1", "--wait"]),
    )
    .unwrap();

    // Assert
    assert!(outcome.success);
    assert_eq!(outcome.value["instance_name"], "peer-3");
}

#[test]
fn governance_confirm_resolves_the_party_and_sends_the_frontend_body() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/decentralized-parties"))
            .and(query_param("prefix", "alpha"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "parties": [{ "party_id": "alpha::1220aa", "threshold": 2 }],
            })))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/governance/confirmations"))
            .and(query_param("party_id", "alpha::1220aa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "threshold": 2,
                "rules_contract_id": "rules-1",
                "member_party_id": "me::1220",
                "actions": [],
                "domain_actions": [{
                    "proposal_cid": "p-1",
                    "action_label": "SetupUtility",
                    "confirmations": [],
                    "confirmation_count": 0,
                    "can_execute": false,
                }],
            })))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/governance/confirm"))
            .and(body_json(json!({
                "party_id": "alpha::1220aa",
                "rules_contract_id": "rules-1",
                "action": { "type": "governance_set_threshold", "new_threshold": 0 },
                "governance_type": "core_domain",
                "proposal_cid": "p-1",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "message": "confirmed" })),
            )
            .expect(1),
    );

    // Act
    let outcome = command::run(
        &mut fake.client(),
        parse(&[
            "governance",
            "confirm",
            "--party",
            "alpha",
            "--proposal",
            "p-1",
        ]),
    )
    .unwrap();

    // Assert
    assert_eq!(outcome.value, json!({ "message": "confirmed" }));
}
