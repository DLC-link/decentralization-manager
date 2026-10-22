//! The API client and the commands against a fake IdP and a fake decman node.

use clap::Parser;
use reqwest::Method;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::api::{AuthSettings, DecmanClient};
use crate::auth::{AuthTarget, PasswordSource, TokenCache, TokenSet};
use crate::cli::Cli;
use crate::command::{self, Command, NodeSettings};
use crate::config::{AdminClient, PartyCredentials};

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

fn run_command(fake: &Fake, command: Command) -> anyhow::Result<command::Outcome> {
    command::run(&mut fake.client(), command, &NodeSettings::default())
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
    let cache = TokenCache::in_dir(&dir.path().join("cache"), &fake.uri(), "alice");

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
    let cache = TokenCache::in_dir(&dir.path().join("cache"), &fake.uri(), "alice");
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
    let cache = TokenCache::in_dir(&dir.path().join("cache"), &fake.uri(), "alice");
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
    let cache = TokenCache::in_dir(&dir.path().join("cache"), &fake.uri(), "alice");
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
    let cache = TokenCache::in_dir(&dir.path().join("cache"), &fake.uri(), "alice");
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
    let cache = TokenCache::in_dir(&dir.path().join("cache"), &fake.uri(), "alice");
    let stale = AuthTarget {
        token_url: format!("{}/old-token", fake.uri()),
        client_id: "cli".to_owned(),
        audience: None,
    };
    cache.save(&stale, &tokens("a-old", 1, "r-old")).unwrap();
    fake.mount(
        // Only the refresh goes to the old endpoint. The password must not.
        Mock::given(path("/old-token"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1),
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
fn an_html_page_for_a_missing_route_is_an_error() {
    // Arrange — the server's SPA fallback answers an unknown route.
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/invitations/handled"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<!doctype html><html></html>", "text/html"),
            )
            .expect(1),
    );

    // Act
    let error = fake
        .client()
        .request(Method::GET, "/invitations/handled", &[], None)
        .unwrap_err();

    // Assert
    assert!(error.to_string().contains("no such route"), "{error}");
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
    let outcome = run_command(&fake, command).unwrap();

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
    let outcome = run_command(&fake, parse(&["workflows", "wait", "dars-7"])).unwrap();

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
    let outcome = run_command(&fake, parse(&["invitations", "accept", "inv-1", "--wait"])).unwrap();

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
    let outcome = run_command(
        &fake,
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

#[cfg(unix)]
#[test]
fn grant_rights_takes_the_admin_client_from_the_profile() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/decentralized-parties"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "parties": [{ "party_id": "alpha::1220aa", "threshold": 2 }],
            })))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/auth/grant-rights"))
            .and(body_json(json!({
                "dec_party_id": "alpha::1220aa",
                "admin_client_id": "participant-admin",
                "admin_client_secret": "admin-s3cret",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .expect(1),
    );
    let settings = NodeSettings {
        admin_client: Some(AdminClient {
            client_id: "participant-admin".to_owned(),
            client_secret_command: "printf 'admin-s3cret\\n'".to_owned(),
        }),
        party_credentials: None,
    };

    // Act
    let outcome = command::run(
        &mut fake.client(),
        parse(&["auth", "grant-rights", "--party", "alpha"]),
        &settings,
    )
    .unwrap();

    // Assert
    assert_eq!(outcome.value, json!({ "ok": true }));
}

#[cfg(unix)]
#[test]
fn party_config_set_builds_the_body_from_the_profile_credentials() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/decentralized-parties"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "parties": [{ "party_id": "alpha::1220aa", "threshold": 2 }],
            })))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("PUT"))
            .and(path("/party-config"))
            .and(body_json(json!({
                "dec_party_id": "alpha::1220aa",
                "member_party_id": "attestor-1::1220bb",
                "user_id": "attestor-user",
                "keycloak_url": "https://idp",
                "keycloak_realm": "nodes",
                "keycloak_client_id": "attestor-1",
                "keycloak_client_secret": "party-s3cret",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .expect(1),
    );
    let settings = NodeSettings {
        admin_client: None,
        party_credentials: Some(PartyCredentials {
            member_party_id: "attestor-1::1220bb".to_owned(),
            user_id: "attestor-user".to_owned(),
            keycloak_url: Some("https://idp".to_owned()),
            keycloak_realm: Some("nodes".to_owned()),
            keycloak_client_id: Some("attestor-1".to_owned()),
            auth0_domain: None,
            auth0_audience: None,
            auth0_client_id: None,
            client_secret_command: Some("printf 'party-s3cret\\n'".to_owned()),
        }),
    };

    // Act
    let outcome = command::run(
        &mut fake.client(),
        parse(&["party-config", "set", "--party", "alpha"]),
        &settings,
    )
    .unwrap();

    // Assert
    assert_eq!(outcome.value, json!({ "ok": true }));
}

fn party_list(id: &str, threshold: i64, participants: &[&str]) -> ResponseTemplate {
    let participants: Vec<Value> = participants
        .iter()
        .map(|participant| json!({ "participant_uid": participant, "permission": "confirmation" }))
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({
        "parties": [{ "party_id": id, "threshold": threshold, "participants": participants }],
    }))
}

#[test]
fn wait_polls_until_the_run_ends() {
    // Arrange — the first poll sees the run in progress, the second completed.
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/workflows"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "runs": [run_json("inprogress", json!({}))] })),
            )
            .up_to_n_times(1)
            .expect(1),
    );
    fake.get_with_token(
        "/workflows",
        "a-1",
        json!({ "runs": [run_json("completed", json!({}))] }),
        1,
    );

    // Act
    let outcome = run_command(
        &fake,
        parse(&["workflows", "wait", "dars-7", "--poll-interval", "1"]),
    )
    .unwrap();

    // Assert
    assert!(outcome.success);
    assert_eq!(outcome.value["status"], "completed");
}

#[test]
fn wait_stops_when_the_run_stays_missing() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.get_with_token("/workflows", "a-1", json!({ "runs": [] }), 5);

    // Act
    let error = run_command(
        &fake,
        parse(&["workflows", "wait", "dars-7", "--poll-interval", "1"]),
    )
    .unwrap_err();

    // Assert
    assert!(error.to_string().contains("dismissed"), "{error}");
}

#[test]
fn wait_stops_at_its_timeout() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.get_with_token(
        "/workflows",
        "a-1",
        json!({ "runs": [run_json("inprogress", json!({}))] }),
        1,
    );

    // Act
    let error = run_command(
        &fake,
        parse(&["workflows", "wait", "dars-7", "--timeout", "0"]),
    )
    .unwrap_err();

    // Assert
    assert!(error.to_string().contains("timed out"), "{error}");
}

#[test]
fn the_tui_calls_refresh_after_a_401_too() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/decentralized-parties"))
            .and(header("authorization", "Bearer a-1"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1),
    );
    fake.token_grant("refresh_token", "a-2", 1);
    fake.get_with_token("/decentralized-parties", "a-2", json!({ "parties": [] }), 1);

    // Act
    let parties = fake.client().fetch_parties().unwrap();

    // Assert
    assert!(parties.is_empty());
}

#[test]
fn a_node_without_auth_gets_no_token_and_no_cache() {
    // Arrange
    let fake = Fake::start();
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/auth-config"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "auth_required": false })),
            )
            .expect(1),
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0),
    );
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/node-config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .expect(1),
    );
    let dir = tempfile::tempdir().unwrap();
    let cache = TokenCache::in_dir(&dir.path().join("cache"), &fake.uri(), "alice");
    let mut auth = fake.auth();
    auth.token_url = None;
    auth.client_id = None;

    // Act
    let result = DecmanClient::new(fake.uri(), auth)
        .unwrap()
        .with_token_cache(cache.clone())
        .request(Method::GET, "/node-config", &[], None);

    // Assert
    assert_eq!(result.unwrap(), json!({ "ok": true }));
    assert!(cache.load().is_none());
}

#[test]
fn plain_http_to_another_host_is_refused() {
    let error = DecmanClient::new("http://decman.example".to_owned(), Fake::start().auth())
        .err()
        .unwrap();
    assert!(error.to_string().contains("use https"), "{error}");
}

#[test]
fn add_member_sends_the_current_threshold_as_previous() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/decentralized-parties"))
            .respond_with(party_list("alpha::1220aa", 2, &["PAR::1", "PAR::2"]))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/add-party"))
            .and(body_json(json!({
                "decentralized_party_id": "alpha::1220aa",
                "new_participant_id": "PAR::3",
                "new_threshold": 2,
                "previous_threshold": 2,
            })))
            .respond_with(
                ResponseTemplate::new(202).set_body_json(json!({ "instance_name": "add-1" })),
            )
            .expect(1),
    );

    // Act
    let outcome = run_command(
        &fake,
        parse(&[
            "parties",
            "add-member",
            "--party",
            "alpha",
            "--participant",
            "PAR::3",
            "--threshold",
            "2",
        ]),
    )
    .unwrap();

    // Assert
    assert_eq!(outcome.value["instance_name"], "add-1");
}

#[test]
fn a_party_the_node_does_not_list_has_no_previous_threshold() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.get_with_token("/decentralized-parties", "a-1", json!({ "parties": [] }), 1);
    fake.mount(
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0),
    );

    // Act
    let error = run_command(
        &fake,
        parse(&[
            "parties",
            "set-threshold",
            "--party",
            "alpha::1220aa",
            "--threshold",
            "1",
        ]),
    )
    .unwrap_err();

    // Assert
    assert!(error.to_string().contains("threshold"), "{error}");
}

#[test]
fn keep_secret_sends_the_body_without_a_secret() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    let body = json!({ "dec_party_id": "alpha::1220aa", "keycloak_client_id": "attestor-1" });
    fake.mount(
        Mock::given(method("PUT"))
            .and(path("/party-config"))
            .and(body_json(body.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .expect(1),
    );
    let body = body.to_string();

    // Act — no profile settings, so a secret lookup would fail.
    let outcome = run_command(
        &fake,
        parse(&["party-config", "set", "--body", &body, "--keep-secret"]),
    )
    .unwrap();

    // Assert
    assert_eq!(outcome.value, json!({ "ok": true }));
}

#[test]
fn deploy_governance_dry_run_reads_but_never_deploys() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/decentralized-parties"))
            .respond_with(party_list("alpha::1220aa", 2, &["PAR::1", "PAR::2"]))
            .expect(1),
    );
    fake.get_with_token(
        "/packages",
        "a-1",
        json!({ "governance_core": "#governance-core-v1" }),
        1,
    );
    fake.get_with_token(
        "/governance/known-members",
        "a-1",
        json!({ "members": [
            { "participant_uid": "PAR::1", "member_party_id": "m1::1220" },
            { "participant_uid": "PAR::2", "member_party_id": "m2::1220" },
        ] }),
        1,
    );
    fake.get_with_token(
        "/operator-info",
        "a-1",
        json!({ "party_id": "op::1220" }),
        1,
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/contracts"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0),
    );

    // Act
    let outcome = run_command(
        &fake,
        parse(&[
            "contracts",
            "deploy-governance",
            "--party",
            "alpha",
            "--threshold",
            "2",
            "--confirmation-timeout-micros",
            "86400000000",
            "--dry-run",
        ]),
    )
    .unwrap();

    // Assert
    assert_eq!(
        outcome.value["participant_parties"],
        json!(["m1::1220", "m2::1220"])
    );
    assert_eq!(outcome.value["operator_party"], "op::1220");
    assert_eq!(
        outcome.value["contracts"][0]["package_id"],
        "#governance-core-v1"
    );
}

#[test]
fn confirm_finds_a_proposal_on_a_later_page() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/decentralized-parties"))
            .respond_with(party_list("alpha::1220aa", 2, &["PAR::1"]))
            .expect(1),
    );
    let page = |cid: &str, next: Option<&str>| {
        json!({
            "threshold": 2,
            "rules_contract_id": "rules-1",
            "member_party_id": "me::1220",
            "actions": [],
            "next_cursor": next,
            "domain_actions": [{
                "proposal_cid": cid, "action_label": "SetupUtility",
                "confirmations": [], "confirmation_count": 0, "can_execute": false,
            }],
        })
    };
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/governance/confirmations"))
            .and(query_param("cursor", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page("p-old", None)))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/governance/confirmations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page("p-new", Some("page-2"))))
            .expect(1),
    );
    fake.mount(
        Mock::given(method("POST"))
            .and(path("/governance/confirm"))
            .and(body_string_contains("p-old"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "message": "ok" })))
            .expect(1),
    );

    // Act
    let outcome = run_command(
        &fake,
        parse(&[
            "governance",
            "confirm",
            "--party",
            "alpha",
            "--proposal",
            "p-old",
        ]),
    )
    .unwrap();

    // Assert
    assert_eq!(outcome.value, json!({ "message": "ok" }));
}

#[test]
fn an_invitation_dar_is_written_as_raw_bytes() {
    // Arrange
    let fake = Fake::start();
    fake.token_grant("password", "a-1", 1);
    let dar = vec![0x50, 0x4b, 0x03, 0x04, 0xff, 0x00];
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/invitations/inv-1/dars/0"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(dar.clone()))
            .expect(1),
    );
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pkg.dar");
    let out_arg = out.display().to_string();

    // Act
    let outcome = run_command(
        &fake,
        parse(&["invitations", "dar", "inv-1", "0", "--out", &out_arg]),
    )
    .unwrap();

    // Assert
    assert_eq!(std::fs::read(&out).unwrap(), dar);
    assert_eq!(outcome.value["bytes"], 6);
}
