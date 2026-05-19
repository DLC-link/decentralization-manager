#![allow(dead_code)]

pub mod auth;
pub mod chaos;
pub mod db;
pub mod governance;
pub mod http;
pub mod invitations;
pub mod operator;
pub mod phases;
pub mod processes;
pub mod scenario;
pub mod types;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context;
use reqwest::Client;

#[derive(Debug, Clone)]
pub struct MemberCreds {
    pub party_id: String,
    pub user_id: String,
    pub keycloak_client_id: String,
    pub keycloak_client_secret: String,
}

/// Keycloak client_credentials for the participant-admin (Canton
/// ParticipantAdmin) service account. Used on devnet by the test runner to
/// drive DPM's POST /auth/grant-rights — that handler mints an admin token
/// from these creds and calls UserManagementService.GrantUserRights to grant
/// CoordinatorUser/attestorUserN the act_as+read_as rights on the freshly-
/// created decentralized party. Localnet uses the JSON Ledger API and
/// ledger-api-user instead, so these are None on that target.
#[derive(Debug, Clone)]
pub struct ParticipantAdminCreds {
    pub keycloak_client_id: String,
    pub keycloak_client_secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestTarget {
    Localnet,
    Devnet,
}

impl TestTarget {
    fn from_env() -> anyhow::Result<Self> {
        match std::env::var("DPM_IT_TARGET").as_deref() {
            Ok("localnet") | Err(_) => Ok(TestTarget::Localnet),
            Ok("devnet") => Ok(TestTarget::Devnet),
            Ok(other) => {
                anyhow::bail!("invalid DPM_IT_TARGET value: {other}; expected localnet|devnet")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodePorts {
    pub http: u16,
    pub noise: u16,
    pub participant_id: String,
}

#[derive(Debug)]
pub struct Fixture {
    pub client: Client,
    pub refresher: Arc<auth::Refresher>,
    pub dev_dir: PathBuf,
    pub p1: NodePorts,
    pub p2: NodePorts,
    pub p3: NodePorts,

    /// Live PID for each participant. Initialized from `P{1,2,3}_PID` env
    /// vars at boot; restart helpers update the slot in place so subsequent
    /// chaos tests target the freshly-spawned process.
    pub current_pids: [Option<u32>; 3],

    pub target: TestTarget,
    pub run_id: String,
    pub operator_party: Option<String>,
    pub dso_party: Option<String>,
    pub operator_timeout_tripped: bool,

    pub party_id: Option<String>,
    pub party_prefix: Option<String>,
    pub rules_contract_id: Option<String>,
    pub p1_member_party: Option<String>,
    pub p2_member_party: Option<String>,
    pub p3_member_party: Option<String>,
    pub provider_service_cid: Option<String>,
    pub allocation_factory_cid: Option<String>,
    pub instrument_configuration_cid: Option<String>,

    pub p1_member_creds: Option<MemberCreds>,
    pub p2_member_creds: Option<MemberCreds>,
    pub p3_member_creds: Option<MemberCreds>,

    pub p1_participant_admin_creds: Option<ParticipantAdminCreds>,
    pub p2_participant_admin_creds: Option<ParticipantAdminCreds>,
    pub p3_participant_admin_creds: Option<ParticipantAdminCreds>,
}

fn read_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("env var {key} not set"))
}

fn read_port(key: &str) -> anyhow::Result<u16> {
    let raw = read_env(key)?;
    raw.parse::<u16>()
        .with_context(|| format!("env var {key} is not a u16: {raw}"))
}

impl Fixture {
    pub fn from_env() -> anyhow::Result<Self> {
        let p1 = NodePorts {
            http: read_port("P1_HTTP")?,
            noise: read_port("P1_NOISE")?,
            participant_id: read_env("P1_PARTICIPANT_ID")?,
        };
        let p2 = NodePorts {
            http: read_port("P2_HTTP")?,
            noise: read_port("P2_NOISE")?,
            participant_id: read_env("P2_PARTICIPANT_ID")?,
        };
        let p3 = NodePorts {
            http: read_port("P3_HTTP")?,
            noise: read_port("P3_NOISE")?,
            participant_id: read_env("P3_PARTICIPANT_ID")?,
        };
        let dev_dir = PathBuf::from(read_env("DEV_DIR")?);
        let current_pids = [
            std::env::var("P1_PID").ok().and_then(|s| s.parse().ok()),
            std::env::var("P2_PID").ok().and_then(|s| s.parse().ok()),
            std::env::var("P3_PID").ok().and_then(|s| s.parse().ok()),
        ];
        let target = TestTarget::from_env()?;
        // Match the bash harness's `curl -s` (no client-side timeout). Some
        // governance proposes against Canton can take >30s when state has
        // accumulated across phases; the e2e itself caps total time so a
        // hung server still fails — we just don't want a per-request 30s
        // ceiling that's tighter than bash's behavior.
        let client = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .context("build reqwest client")?;
        let refresher = match target {
            TestTarget::Localnet => Arc::new(auth::Refresher::Static {
                token: read_env("MOCK_TOKEN")?,
            }),
            TestTarget::Devnet => {
                let creds = auth::KeycloakCreds {
                    url: read_env("DECPM_KEYCLOAK_URL")?,
                    realm: read_env("DECPM_KEYCLOAK_REALM")?,
                    client_id: read_env("DECPM_KEYCLOAK_CLIENT_ID")?,
                    username: read_env("DECPM_KEYCLOAK_USERNAME")?,
                    password: read_env("DECPM_KEYCLOAK_PASSWORD")?,
                };
                Arc::new(auth::Refresher::Keycloak {
                    client: client.clone(),
                    creds,
                    state: tokio::sync::Mutex::new(auth::TokenState::expired()),
                })
            }
        };
        let run_id = std::env::var("DPM_IT_RUN_ID").unwrap_or_else(|_| {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("dpm-it-{ts}-{pid}", pid = std::process::id())
        });

        let (
            p1_member_creds,
            p2_member_creds,
            p3_member_creds,
            p1_participant_admin_creds,
            p2_participant_admin_creds,
            p3_participant_admin_creds,
        ) = match target {
            TestTarget::Devnet => {
                let read_member_creds = |n: u8| -> anyhow::Result<MemberCreds> {
                    Ok(MemberCreds {
                        party_id: read_env(&format!("P{n}_MEMBER_PARTY_ID"))?,
                        user_id: read_env(&format!("P{n}_MEMBER_USER_ID"))?,
                        keycloak_client_id: read_env(&format!("P{n}_MEMBER_KEYCLOAK_CLIENT_ID"))?,
                        keycloak_client_secret: read_env(&format!(
                            "P{n}_MEMBER_KEYCLOAK_CLIENT_SECRET"
                        ))?,
                    })
                };
                let read_admin_creds = |n: u8| -> anyhow::Result<ParticipantAdminCreds> {
                    Ok(ParticipantAdminCreds {
                        keycloak_client_id: read_env(&format!(
                            "P{n}_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID"
                        ))?,
                        keycloak_client_secret: read_env(&format!(
                            "P{n}_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET"
                        ))?,
                    })
                };
                (
                    Some(read_member_creds(1)?),
                    Some(read_member_creds(2)?),
                    Some(read_member_creds(3)?),
                    Some(read_admin_creds(1)?),
                    Some(read_admin_creds(2)?),
                    Some(read_admin_creds(3)?),
                )
            }
            TestTarget::Localnet => (None, None, None, None, None, None),
        };

        Ok(Fixture {
            client,
            refresher,
            dev_dir,
            p1,
            p2,
            p3,
            current_pids,
            target,
            run_id,
            operator_party: None,
            dso_party: None,
            operator_timeout_tripped: false,
            party_id: None,
            party_prefix: None,
            rules_contract_id: None,
            p1_member_party: None,
            p2_member_party: None,
            p3_member_party: None,
            provider_service_cid: None,
            allocation_factory_cid: None,
            instrument_configuration_cid: None,
            p1_member_creds,
            p2_member_creds,
            p3_member_creds,
            p1_participant_admin_creds,
            p2_participant_admin_creds,
            p3_participant_admin_creds,
        })
    }

    pub async fn discover_network_parties(&mut self) -> anyhow::Result<()> {
        // On localnet, the test substitutes p1_member for the DSO party (token_custody)
        // and never needs a real operator party (utility_onboarding uses the
        // ProvisionProviderService shortcut). Discovery is only meaningful on devnet
        // where the DSO and utility-registry operator are real, separate parties.
        if self.target == TestTarget::Localnet {
            return Ok(());
        }
        let net: types::NetworkInfoResponse = self
            .get_json(self.p1.http, "/network-info")
            .await
            .context("GET /network-info")?;
        self.dso_party = Some(net.dso_party_id);

        let op: types::OperatorInfoResponse = self
            .get_json(self.p1.http, "/operator-info")
            .await
            .context("GET /operator-info")?;
        self.operator_party = Some(op.party_id);
        Ok(())
    }

    pub fn party_id(&self) -> anyhow::Result<&str> {
        self.party_id
            .as_deref()
            .context("party_id not set — create_dec_party must run first")
    }
    pub fn party_prefix(&self) -> anyhow::Result<&str> {
        self.party_prefix
            .as_deref()
            .context("party_prefix not set — create_dec_party must run first")
    }
    pub fn rules_contract_id(&self) -> anyhow::Result<&str> {
        self.rules_contract_id
            .as_deref()
            .context("rules_contract_id not set — deploy_gov_core must run first")
    }
    pub fn p1_member_party(&self) -> anyhow::Result<&str> {
        self.p1_member_party
            .as_deref()
            .context("p1_member_party not set")
    }
    pub fn p2_member_party(&self) -> anyhow::Result<&str> {
        self.p2_member_party
            .as_deref()
            .context("p2_member_party not set")
    }
    pub fn p3_member_party(&self) -> anyhow::Result<&str> {
        self.p3_member_party
            .as_deref()
            .context("p3_member_party not set")
    }

    /// Build a `Fixture` with hardcoded test values, bypassing env vars entirely.
    /// Used by unit tests for the Scenario DSL — those tests don't make HTTP calls,
    /// they only need a `Fixture` instance to pass to step closures.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::for_test_with_jwt("test-jwt")
    }

    /// Like `for_test()` but with a custom JWT string for tests that assert on
    /// specific bearer token values in HTTP requests.
    #[cfg(test)]
    pub fn for_test_with_jwt(jwt: &str) -> Self {
        Self {
            client: Client::builder()
                .build()
                .expect("build reqwest client for test"),
            refresher: Arc::new(auth::Refresher::Static {
                token: jwt.to_string(),
            }),
            dev_dir: PathBuf::from("/tmp/dpm-it-test"),
            current_pids: [None, None, None],
            p1: NodePorts {
                http: 8081,
                noise: 9001,
                participant_id: "p1".to_string(),
            },
            p2: NodePorts {
                http: 8082,
                noise: 9002,
                participant_id: "p2".to_string(),
            },
            p3: NodePorts {
                http: 8083,
                noise: 9003,
                participant_id: "p3".to_string(),
            },
            target: TestTarget::Localnet,
            run_id: "test-run-id".to_string(),
            operator_party: None,
            dso_party: None,
            operator_timeout_tripped: false,
            party_id: None,
            party_prefix: None,
            rules_contract_id: None,
            p1_member_party: None,
            p2_member_party: None,
            p3_member_party: None,
            provider_service_cid: None,
            allocation_factory_cid: None,
            instrument_configuration_cid: None,
            p1_member_creds: None,
            p2_member_creds: None,
            p3_member_creds: None,
            p1_participant_admin_creds: None,
            p2_participant_admin_creds: None,
            p3_participant_admin_creds: None,
        }
    }
}

#[cfg(test)]
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn set_all_env() {
        unsafe {
            std::env::set_var("P1_HTTP", "8081");
            std::env::set_var("P2_HTTP", "8082");
            std::env::set_var("P3_HTTP", "8083");
            std::env::set_var("P1_NOISE", "9001");
            std::env::set_var("P2_NOISE", "9002");
            std::env::set_var("P3_NOISE", "9003");
            std::env::set_var("P1_PARTICIPANT_ID", "p1");
            std::env::set_var("P2_PARTICIPANT_ID", "p2");
            std::env::set_var("P3_PARTICIPANT_ID", "p3");
            std::env::set_var("MOCK_TOKEN", "mock-jwt");
            std::env::set_var("DEV_DIR", "/tmp/dpm-it-test");
            std::env::set_var("DPM_IT_RUN_ID", "test-run-id");
            // DPM_IT_TARGET intentionally left unset — exercises the default path
        }
    }

    fn set_devnet_env() {
        unsafe {
            std::env::set_var("DPM_IT_TARGET", "devnet");
            std::env::set_var("DECPM_KEYCLOAK_URL", "https://keycloak.example.com/auth");
            std::env::set_var("DECPM_KEYCLOAK_REALM", "test-realm");
            std::env::set_var("DECPM_KEYCLOAK_CLIENT_ID", "test-client");
            std::env::set_var("DECPM_KEYCLOAK_USERNAME", "testuser");
            std::env::set_var("DECPM_KEYCLOAK_PASSWORD", "testpass");
            // Per-participant member-party credentials
            std::env::set_var("P1_MEMBER_PARTY_ID", "p1-member-party");
            std::env::set_var("P1_MEMBER_USER_ID", "p1-user");
            std::env::set_var("P1_MEMBER_KEYCLOAK_CLIENT_ID", "p1-client-id");
            std::env::set_var("P1_MEMBER_KEYCLOAK_CLIENT_SECRET", "p1-secret");
            std::env::set_var("P2_MEMBER_PARTY_ID", "p2-member-party");
            std::env::set_var("P2_MEMBER_USER_ID", "p2-user");
            std::env::set_var("P2_MEMBER_KEYCLOAK_CLIENT_ID", "p2-client-id");
            std::env::set_var("P2_MEMBER_KEYCLOAK_CLIENT_SECRET", "p2-secret");
            std::env::set_var("P3_MEMBER_PARTY_ID", "p3-member-party");
            std::env::set_var("P3_MEMBER_USER_ID", "p3-user");
            std::env::set_var("P3_MEMBER_KEYCLOAK_CLIENT_ID", "p3-client-id");
            std::env::set_var("P3_MEMBER_KEYCLOAK_CLIENT_SECRET", "p3-secret");
            // Per-participant admin Keycloak creds (required by Fixture::from_env on devnet).
            std::env::set_var("P1_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID", "p1-admin-client");
            std::env::set_var(
                "P1_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET",
                "p1-admin-secret",
            );
            std::env::set_var("P2_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID", "p2-admin-client");
            std::env::set_var(
                "P2_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET",
                "p2-admin-secret",
            );
            std::env::set_var("P3_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID", "p3-admin-client");
            std::env::set_var(
                "P3_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET",
                "p3-admin-secret",
            );
        }
    }

    fn clear_all_env() {
        unsafe {
            for k in [
                "P1_HTTP",
                "P2_HTTP",
                "P3_HTTP",
                "P1_NOISE",
                "P2_NOISE",
                "P3_NOISE",
                "P1_PARTICIPANT_ID",
                "P2_PARTICIPANT_ID",
                "P3_PARTICIPANT_ID",
                "MOCK_TOKEN",
                "DEV_DIR",
                "DPM_IT_RUN_ID",
                "DPM_IT_TARGET",
                "DECPM_KEYCLOAK_URL",
                "DECPM_KEYCLOAK_REALM",
                "DECPM_KEYCLOAK_CLIENT_ID",
                "DECPM_KEYCLOAK_USERNAME",
                "DECPM_KEYCLOAK_PASSWORD",
                "P1_MEMBER_PARTY_ID",
                "P1_MEMBER_USER_ID",
                "P1_MEMBER_KEYCLOAK_CLIENT_ID",
                "P1_MEMBER_KEYCLOAK_CLIENT_SECRET",
                "P2_MEMBER_PARTY_ID",
                "P2_MEMBER_USER_ID",
                "P2_MEMBER_KEYCLOAK_CLIENT_ID",
                "P2_MEMBER_KEYCLOAK_CLIENT_SECRET",
                "P3_MEMBER_PARTY_ID",
                "P3_MEMBER_USER_ID",
                "P3_MEMBER_KEYCLOAK_CLIENT_ID",
                "P3_MEMBER_KEYCLOAK_CLIENT_SECRET",
                "P1_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID",
                "P1_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET",
                "P2_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID",
                "P2_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET",
                "P3_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID",
                "P3_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET",
            ] {
                std::env::remove_var(k);
            }
        }
    }

    #[tokio::test]
    async fn from_env_succeeds_when_all_vars_present() {
        let f = {
            let _g = ENV_LOCK.lock().unwrap();
            clear_all_env();
            set_all_env();
            Fixture::from_env().unwrap()
        };
        assert_eq!(f.p1.http, 8081);
        assert_eq!(f.p3.noise, 9003);
        assert_eq!(f.p2.participant_id, "p2");
        assert_eq!(f.refresher.token().await.unwrap(), "mock-jwt");
    }

    #[test]
    fn from_env_reports_missing_var() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        unsafe { std::env::remove_var("P2_HTTP") };
        let err = Fixture::from_env().unwrap_err();
        assert!(format!("{err:#}").contains("P2_HTTP"));
    }

    #[test]
    fn from_env_reports_invalid_port() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        unsafe { std::env::set_var("P1_NOISE", "not-a-port") };
        let err = Fixture::from_env().unwrap_err();
        assert!(format!("{err:#}").contains("P1_NOISE"));
    }

    #[test]
    fn target_defaults_to_localnet_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        let f = Fixture::from_env().unwrap();
        assert!(matches!(f.target, TestTarget::Localnet));
    }

    #[test]
    fn target_parses_devnet() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        set_devnet_env();
        let f = Fixture::from_env().unwrap();
        assert!(matches!(f.target, TestTarget::Devnet));
    }

    #[test]
    fn target_parses_localnet_explicitly() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        unsafe { std::env::set_var("DPM_IT_TARGET", "localnet") };
        let f = Fixture::from_env().unwrap();
        assert!(matches!(f.target, TestTarget::Localnet));
    }

    #[test]
    fn target_errors_on_invalid_value() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        unsafe { std::env::set_var("DPM_IT_TARGET", "stagnet") };
        let err = Fixture::from_env().unwrap_err();
        assert!(format!("{err:#}").contains("DPM_IT_TARGET"));
        assert!(format!("{err:#}").contains("stagnet"));
        assert!(format!("{err:#}").contains("localnet"));
        assert!(format!("{err:#}").contains("devnet"));
    }

    #[test]
    fn run_id_read_from_env() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        let f = Fixture::from_env().unwrap();
        assert_eq!(f.run_id, "test-run-id");
    }

    #[test]
    fn run_id_generated_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env();
        set_all_env();
        unsafe { std::env::remove_var("DPM_IT_RUN_ID") };
        let f = Fixture::from_env().unwrap();
        assert!(f.run_id.starts_with("dpm-it-"));
    }

    #[test]
    fn for_test_returns_localnet_fixture_with_defaults() {
        let f = Fixture::for_test();
        assert!(matches!(f.target, TestTarget::Localnet));
        assert!(f.operator_party.is_none());
        assert!(f.dso_party.is_none());
        assert!(!f.operator_timeout_tripped);
        assert!(!f.run_id.is_empty());
    }

    #[tokio::test]
    async fn for_test_refresher_returns_test_jwt() {
        let f = Fixture::for_test();
        let token = f.refresher.token().await.unwrap();
        assert_eq!(token, "test-jwt");
    }

    #[tokio::test]
    async fn discover_network_parties_populates_devnet_fields() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/network-info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dso_party_id": "DSO::1220abc"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/operator-info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "party_id": "Operator::1220def"
            })))
            .mount(&server)
            .await;

        let mut f = Fixture::for_test();
        f.p1.http = server.address().port();
        f.target = TestTarget::Devnet;

        f.discover_network_parties().await.unwrap();
        assert_eq!(f.dso_party.as_deref(), Some("DSO::1220abc"));
        assert_eq!(f.operator_party.as_deref(), Some("Operator::1220def"));
    }

    #[tokio::test]
    async fn discover_network_parties_noop_on_localnet() {
        let mut f = Fixture::for_test();
        f.target = TestTarget::Localnet;
        // No mock server — would panic / fail on any HTTP call.
        f.discover_network_parties().await.unwrap();
        assert!(f.dso_party.is_none());
        assert!(f.operator_party.is_none());
    }
}
