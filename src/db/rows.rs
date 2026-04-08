use crate::{
    config::{KeycloakConfig, PackageConfig, PartyCredentials, Peer},
    error::Result,
    participant_id::CantonId,
};

#[derive(Debug, sqlx::FromRow)]
pub struct PeerRow {
    pub participant_id: String,
    pub name: String,
    pub address: String,
    pub port: i64,
    pub public_key: String,
    pub party: Option<String>,
}

impl PeerRow {
    pub fn from_domain(peer: &Peer) -> Self {
        Self {
            participant_id: peer.participant_id.to_string(),
            name: peer.name.clone(),
            address: peer.address.clone(),
            port: peer.port as i64,
            public_key: peer.public_key.clone(),
            party: peer.party.clone(),
        }
    }

    pub fn into_domain(self) -> Result<Peer> {
        Ok(Peer {
            participant_id: CantonId::parse(&self.participant_id)?,
            name: self.name,
            address: self.address,
            port: self.port as u16,
            public_key: self.public_key,
            party: self.party,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct PartyCredentialsRow {
    pub dec_party_id: String,
    pub member_party_id: String,
    pub user_id: String,
    pub keycloak_url: String,
    pub keycloak_realm: String,
    pub keycloak_client_id: String,
    pub keycloak_client_secret: Option<String>,
    pub keycloak_username: Option<String>,
    pub keycloak_password: Option<String>,
    pub governance_core: Option<String>,
    pub governance_token_custody: Option<String>,
    pub utility_credential: Option<String>,
    pub utility_registry: Option<String>,
    pub vault: Option<String>,
    pub vault_governance: Option<String>,
}

impl PartyCredentialsRow {
    pub fn from_domain(creds: &PartyCredentials) -> Self {
        Self {
            dec_party_id: creds.dec_party_id.to_string(),
            member_party_id: creds.member_party_id.to_string(),
            user_id: creds.user_id.clone(),
            keycloak_url: creds.keycloak.url.clone(),
            keycloak_realm: creds.keycloak.realm.clone(),
            keycloak_client_id: creds.keycloak.client_id.clone(),
            keycloak_client_secret: creds.keycloak.client_secret.clone(),
            keycloak_username: creds.keycloak.username.clone(),
            keycloak_password: creds.keycloak.password.clone(),
            governance_core: creds.packages.governance_core.clone(),
            governance_token_custody: creds.packages.governance_token_custody.clone(),
            utility_credential: creds.packages.utility_credential.clone(),
            utility_registry: creds.packages.utility_registry.clone(),
            vault: creds.packages.vault.clone(),
            vault_governance: creds.packages.vault_governance.clone(),
        }
    }

    pub fn into_domain(self) -> Result<PartyCredentials> {
        Ok(PartyCredentials {
            dec_party_id: CantonId::parse(&self.dec_party_id)?,
            member_party_id: CantonId::parse(&self.member_party_id)?,
            user_id: self.user_id,
            keycloak: KeycloakConfig {
                url: self.keycloak_url,
                realm: self.keycloak_realm,
                client_id: self.keycloak_client_id,
                client_secret: self.keycloak_client_secret,
                username: self.keycloak_username,
                password: self.keycloak_password,
            },
            packages: PackageConfig {
                governance_core: self.governance_core,
                governance_token_custody: self.governance_token_custody,
                utility_credential: self.utility_credential,
                utility_registry: self.utility_registry,
                vault: self.vault,
                vault_governance: self.vault_governance,
            },
        })
    }
}
