use std::str::FromStr;

use anyhow::Context;

use crate::{
    config::{KeycloakConfig, PackageConfig, PartyCredentials, Peer},
    db::crypto,
    error::Result,
    participant_id::CantonId,
    server::{InvitationType, PendingInvitation},
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
}

impl PartyCredentialsRow {
    pub fn from_domain(creds: &PartyCredentials) -> Result<Self> {
        Ok(Self {
            dec_party_id: creds.dec_party_id.to_string(),
            member_party_id: creds.member_party_id.to_string(),
            user_id: creds.user_id.clone(),
            keycloak_url: creds.keycloak.url.clone(),
            keycloak_realm: creds.keycloak.realm.clone(),
            keycloak_client_id: crypto::encrypt(&creds.keycloak.client_id)?,
            keycloak_client_secret: crypto::encrypt_opt(&creds.keycloak.client_secret)?,
            keycloak_username: crypto::encrypt_opt(&creds.keycloak.username)?,
            keycloak_password: crypto::encrypt_opt(&creds.keycloak.password)?,
        })
    }

    pub fn into_domain(self) -> Result<PartyCredentials> {
        Ok(PartyCredentials {
            dec_party_id: CantonId::parse(&self.dec_party_id)?,
            member_party_id: CantonId::parse(&self.member_party_id)?,
            user_id: self.user_id,
            keycloak: KeycloakConfig {
                url: self.keycloak_url,
                realm: self.keycloak_realm,
                client_id: crypto::decrypt(&self.keycloak_client_id)?,
                client_secret: crypto::decrypt_opt(self.keycloak_client_secret)?,
                username: crypto::decrypt_opt(self.keycloak_username)?,
                password: crypto::decrypt_opt(self.keycloak_password)?,
            },
            packages: PackageConfig::default(),
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct DecPartyRow {
    pub party_id: String,
    pub prefix: String,
    pub threshold: i64,
    pub updated_at: i64,
    pub my_owner_key: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DecPartyOwnerRow {
    pub dec_party_id: String,
    pub owner_key: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DecPartyParticipantRow {
    pub dec_party_id: String,
    pub participant_uid: String,
    pub permission: String,
    pub owner_key: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DecPartyContractRow {
    pub dec_party_id: String,
    pub contract_id: String,
    pub template_id: String,
    pub package_id: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct GovernanceAuditRow {
    pub id: i64,
    pub timestamp: i64,
    pub event_type: String,
    pub party_id: String,
    pub member_party_id: String,
    pub governance_type: String,
    pub action_summary: String,
    pub details: String,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct PendingInvitationRow {
    pub id: String,
    pub invitation_type: String,
    pub coordinator_pubkey: String,
    pub received_at: i64,
    pub prefix: Option<String>,
    pub participants: Option<String>,
    pub dar_filenames: Option<String>,
}

fn encode_string_list(items: &[String], context_label: &str) -> Result<Option<String>> {
    if items.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(items).with_context(|| {
            format!("failed to encode {context_label}")
        })?))
    }
}

fn decode_string_list(raw: Option<String>, id: &str, context_label: &str) -> Result<Vec<String>> {
    match raw {
        Some(s) if !s.is_empty() => serde_json::from_str(&s)
            .with_context(|| format!("invalid {context_label} JSON for id {id}")),
        _ => Ok(Vec::new()),
    }
}

impl PendingInvitationRow {
    pub fn from_domain(inv: &PendingInvitation) -> Result<Self> {
        Ok(Self {
            id: inv.id.clone(),
            invitation_type: inv.invitation_type.to_string(),
            coordinator_pubkey: inv.coordinator_pubkey.clone(),
            received_at: inv.received_at,
            prefix: inv.prefix.clone(),
            participants: encode_string_list(&inv.participants, "pending invitation participants")?,
            dar_filenames: encode_string_list(
                &inv.dar_filenames,
                "pending invitation dar_filenames",
            )?,
        })
    }

    pub fn into_domain(self) -> Result<PendingInvitation> {
        let invitation_type = InvitationType::from_str(&self.invitation_type)
            .with_context(|| format!("invalid invitation_type for id {}", self.id))?;
        let participants = decode_string_list(self.participants, &self.id, "participants")?;
        let dar_filenames = decode_string_list(self.dar_filenames, &self.id, "dar_filenames")?;
        Ok(PendingInvitation {
            id: self.id,
            invitation_type,
            coordinator_pubkey: self.coordinator_pubkey,
            coordinator_name: None,
            received_at: self.received_at,
            prefix: self.prefix,
            participants,
            dar_filenames,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct ChainAuditCacheRow {
    pub party_id: String,
    pub offset: i64,
    pub timestamp: i64,
    pub event_type: String,
    pub contract_id: String,
    pub template_id: String,
    pub package_id: String,
    pub governance_type: String,
    pub action_summary: String,
    pub choice: Option<String>,
    pub acting_parties: String,
    pub update_id: String,
    pub details: String,
}
