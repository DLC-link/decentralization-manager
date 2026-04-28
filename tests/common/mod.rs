#![allow(dead_code)]

pub mod phases;

use std::sync::Mutex;

use anyhow::Context;
use reqwest::Client;

#[derive(Debug, Clone)]
pub struct NodePorts {
    pub http: u16,
    pub noise: u16,
    pub participant_id: String,
}

#[derive(Debug)]
pub struct Fixture {
    pub client: Client,
    pub jwt: String,
    pub p1: NodePorts,
    pub p2: NodePorts,
    pub p3: NodePorts,

    pub party_id: Option<String>,
    pub party_prefix: Option<String>,
    pub rules_contract_id: Option<String>,
    pub p1_member_party: Option<String>,
    pub p2_member_party: Option<String>,
    pub p3_member_party: Option<String>,
    pub provider_service_cid: Option<String>,
    pub allocation_factory_cid: Option<String>,
    pub instrument_configuration_cid: Option<String>,
}

fn read_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("env var {key} not set"))
}

fn read_port(key: &str) -> anyhow::Result<u16> {
    let raw = read_env(key)?;
    raw.parse::<u16>().with_context(|| format!("env var {key} is not a u16: {raw}"))
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
        let jwt = read_env("MOCK_TOKEN")?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("build reqwest client")?;
        Ok(Fixture {
            client, jwt, p1, p2, p3,
            party_id: None, party_prefix: None, rules_contract_id: None,
            p1_member_party: None, p2_member_party: None, p3_member_party: None,
            provider_service_cid: None, allocation_factory_cid: None,
            instrument_configuration_cid: None,
        })
    }

    pub fn party_id(&self) -> anyhow::Result<&str> {
        self.party_id.as_deref().context("party_id not set — create_dec_party must run first")
    }
    pub fn party_prefix(&self) -> anyhow::Result<&str> {
        self.party_prefix.as_deref().context("party_prefix not set — create_dec_party must run first")
    }
    pub fn rules_contract_id(&self) -> anyhow::Result<&str> {
        self.rules_contract_id.as_deref().context("rules_contract_id not set — deploy_gov_core must run first")
    }
    pub fn p1_member_party(&self) -> anyhow::Result<&str> {
        self.p1_member_party.as_deref().context("p1_member_party not set")
    }
    pub fn p2_member_party(&self) -> anyhow::Result<&str> {
        self.p2_member_party.as_deref().context("p2_member_party not set")
    }
    pub fn p3_member_party(&self) -> anyhow::Result<&str> {
        self.p3_member_party.as_deref().context("p3_member_party not set")
    }

    /// Build a `Fixture` with hardcoded test values, bypassing env vars entirely.
    /// Used by unit tests for the Scenario DSL — those tests don't make HTTP calls,
    /// they only need a `Fixture` instance to pass to step closures.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            client: Client::builder().build().expect("build reqwest client for test"),
            jwt: "test-jwt".to_string(),
            p1: NodePorts { http: 8081, noise: 9001, participant_id: "p1".to_string() },
            p2: NodePorts { http: 8082, noise: 9002, participant_id: "p2".to_string() },
            p3: NodePorts { http: 8083, noise: 9003, participant_id: "p3".to_string() },
            party_id: None, party_prefix: None, rules_contract_id: None,
            p1_member_party: None, p2_member_party: None, p3_member_party: None,
            provider_service_cid: None, allocation_factory_cid: None,
            instrument_configuration_cid: None,
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
        }
    }

    fn clear_all_env() {
        unsafe {
            for k in ["P1_HTTP","P2_HTTP","P3_HTTP","P1_NOISE","P2_NOISE","P3_NOISE",
                      "P1_PARTICIPANT_ID","P2_PARTICIPANT_ID","P3_PARTICIPANT_ID","MOCK_TOKEN"] {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    fn from_env_succeeds_when_all_vars_present() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env(); set_all_env();
        let f = Fixture::from_env().unwrap();
        assert_eq!(f.p1.http, 8081);
        assert_eq!(f.p3.noise, 9003);
        assert_eq!(f.p2.participant_id, "p2");
        assert_eq!(f.jwt, "mock-jwt");
    }

    #[test]
    fn from_env_reports_missing_var() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env(); set_all_env();
        unsafe { std::env::remove_var("P2_HTTP") };
        let err = Fixture::from_env().unwrap_err();
        assert!(format!("{err:#}").contains("P2_HTTP"));
    }

    #[test]
    fn from_env_reports_invalid_port() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_all_env(); set_all_env();
        unsafe { std::env::set_var("P1_NOISE", "not-a-port") };
        let err = Fixture::from_env().unwrap_err();
        assert!(format!("{err:#}").contains("P1_NOISE"));
    }
}
