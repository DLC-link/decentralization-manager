//! Rows and ids shared by the SQLite tests.

use crate::{
    canton_id::CantonId,
    config::{KeycloakConfig, PackageConfig, PartyCredentials, Peer},
    db::rows::DecPartyRow,
};

pub(super) const TEST_NS: &str =
    "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub(super) fn test_peer(index: u8) -> Peer {
    let ns = format!("1220{:0>64}", format!("{index:02x}"));
    Peer {
        participant_id: CantonId::parse(&format!("node{index}::{ns}")).unwrap(),
        name: format!("Node {index}"),
        address: format!("10.0.0.{index}"),
        port: 9000 + index as u16,
        public_key: format!("03{index:02x}abcdef"),
        party: None,
    }
}

pub(super) fn test_creds(prefix: &str) -> PartyCredentials {
    PartyCredentials {
        dec_party_id: CantonId::parse(&format!("{prefix}::{TEST_NS}")).unwrap(),
        member_party_id: CantonId::parse(&format!("member::{TEST_NS}")).unwrap(),
        user_id: "test-user".to_string(),
        keycloak: KeycloakConfig {
            url: "https://kc.example.com".to_string(),
            internal_url: None,
            realm: "test".to_string(),
            client_id: "client-1".to_string(),
            client_secret: Some("secret".to_string()),
            username: None,
            password: None,
        },
        auth0: None,
        packages: PackageConfig {
            governance_action: Some("#gov-action".to_string()),
            governance_core: Some("#gov-core".to_string()),
            governance_rewards: Some("#gov-rewards".to_string()),
            governance_token_custody: None,
            governance_utility_credential: None,
            governance_utility_onboarding: None,
            utility_credential: None,
            utility_credential_app: None,
            utility_registry: None,
        },
    }
}

pub(super) fn test_dec_party(prefix: &str) -> DecPartyRow {
    DecPartyRow {
        party_id: format!("{prefix}::{TEST_NS}"),
        prefix: prefix.to_string(),
        threshold: 2,
        updated_at: 1000,
        my_owner_key: None,
        confirmation_threshold: None,
    }
}
