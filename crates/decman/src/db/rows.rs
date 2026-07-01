use std::str::FromStr;

use anyhow::Context;

use crate::{
    canton_id::CantonId,
    config::{Auth0M2MConfig, KeycloakConfig, PackageConfig, PartyCredentials, Peer},
    db::crypto,
    error::Result,
    server::{
        InvitationType, PendingInvitation, WorkflowKind, WorkflowProgress, WorkflowRole,
        WorkflowRun,
    },
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
    pub auth0_domain: Option<String>,
    pub auth0_audience: Option<String>,
    pub auth0_client_id: Option<String>,
    pub auth0_client_secret: Option<String>,
}

impl PartyCredentialsRow {
    pub fn from_domain(creds: &PartyCredentials) -> Result<Self> {
        let (auth0_domain, auth0_audience, auth0_client_id, auth0_client_secret) =
            match &creds.auth0 {
                Some(a) => (
                    Some(a.domain.clone()),
                    Some(a.audience.clone()),
                    Some(crypto::encrypt(&a.client_id)?),
                    Some(crypto::encrypt(&a.client_secret)?),
                ),
                None => (None, None, None, None),
            };
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
            auth0_domain,
            auth0_audience,
            auth0_client_id,
            auth0_client_secret,
        })
    }

    pub fn into_domain(self) -> Result<PartyCredentials> {
        let auth0 = match (
            self.auth0_domain,
            self.auth0_audience,
            self.auth0_client_id,
            self.auth0_client_secret,
        ) {
            (Some(domain), Some(audience), Some(cid_enc), Some(secret_enc)) => {
                Some(Auth0M2MConfig {
                    domain,
                    audience,
                    client_id: crypto::decrypt(&cid_enc)?,
                    client_secret: crypto::decrypt(&secret_enc)?,
                })
            }
            _ => None,
        };
        Ok(PartyCredentials {
            dec_party_id: CantonId::parse(&self.dec_party_id)?,
            member_party_id: CantonId::parse(&self.member_party_id)?,
            user_id: self.user_id,
            keycloak: KeycloakConfig {
                url: self.keycloak_url,
                // Per-party credentials are not stored with a backchannel URL;
                // the server-side internal URL is an env-configured, inbound
                // (frontend-gating) concern. Falls back to `url`.
                internal_url: None,
                realm: self.keycloak_realm,
                client_id: crypto::decrypt(&self.keycloak_client_id)?,
                client_secret: crypto::decrypt_opt(self.keycloak_client_secret)?,
                username: crypto::decrypt_opt(self.keycloak_username)?,
                password: crypto::decrypt_opt(self.keycloak_password)?,
            },
            auth0,
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
    pub package_name: String,
    pub package_version: String,
    pub created_at: String,
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
    pub kicked_participant: Option<String>,
    pub new_participant: Option<String>,
    pub new_threshold: Option<i64>,
    pub previous_threshold: Option<i64>,
    pub dec_party_id: Option<String>,
    pub package_names: Option<String>,
    pub workflow_instance: Option<String>,
}

fn encode_list<T: serde::Serialize>(items: &[T], context_label: &str) -> Result<Option<String>> {
    if items.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(items).with_context(|| {
            format!("failed to encode {context_label}")
        })?))
    }
}

fn decode_list<T: for<'de> serde::Deserialize<'de>>(
    raw: Option<String>,
    id: &str,
    context_label: &str,
) -> Result<Vec<T>> {
    match raw {
        Some(s) if !s.is_empty() => serde_json::from_str(&s)
            .with_context(|| format!("invalid {context_label} JSON for id {id}")),
        _ => Ok(Vec::new()),
    }
}

impl PendingInvitationRow {
    /// Build a row from the in-memory domain object.
    ///
    /// # Errors
    ///
    /// Returns an error if the participants/dar_filenames lists fail to
    /// JSON-encode.
    pub fn from_domain(inv: &PendingInvitation) -> Result<Self> {
        Ok(Self {
            id: inv.id.clone(),
            invitation_type: inv.invitation_type.to_string(),
            coordinator_pubkey: inv.coordinator_pubkey.clone(),
            received_at: inv.received_at,
            prefix: inv.prefix.clone(),
            participants: encode_list(&inv.participants, "pending invitation participants")?,
            dar_filenames: encode_list(&inv.dar_filenames, "pending invitation dar_filenames")?,
            kicked_participant: inv.kicked_participant.as_ref().map(|p| p.to_string()),
            new_participant: inv.new_participant.as_ref().map(|p| p.to_string()),
            new_threshold: inv.new_threshold.map(i64::from),
            previous_threshold: inv.previous_threshold.map(i64::from),
            dec_party_id: inv.dec_party_id.as_ref().map(|p| p.to_string()),
            package_names: encode_list(&inv.package_names, "pending invitation package_names")?,
            workflow_instance: inv.workflow_instance.clone(),
        })
    }

    /// Decode a row read from the database back into a domain object.
    ///
    /// # Errors
    ///
    /// Returns an error if `invitation_type` is unrecognised or if the
    /// participants/dar_filenames JSON fields are malformed.
    pub fn into_domain(self) -> Result<PendingInvitation> {
        let invitation_type = InvitationType::from_str(&self.invitation_type)
            .with_context(|| format!("invalid invitation_type for id {}", self.id))?;
        let participants = decode_list(self.participants, &self.id, "participants")?;
        let dar_filenames = decode_list(self.dar_filenames, &self.id, "dar_filenames")?;
        let package_names = decode_list(self.package_names, &self.id, "package_names")?;
        let kicked_participant = self
            .kicked_participant
            .map(|s| CantonId::parse(&s))
            .transpose()
            .with_context(|| format!("invalid kicked_participant for id {}", self.id))?;
        let new_participant = self
            .new_participant
            .map(|s| CantonId::parse(&s))
            .transpose()
            .with_context(|| format!("invalid new_participant for id {}", self.id))?;
        let dec_party_id = self
            .dec_party_id
            .map(|s| CantonId::parse(&s))
            .transpose()
            .with_context(|| format!("invalid dec_party_id for id {}", self.id))?;
        Ok(PendingInvitation {
            id: self.id,
            invitation_type,
            coordinator_pubkey: self.coordinator_pubkey,
            coordinator_name: None,
            received_at: self.received_at,
            prefix: self.prefix,
            participants,
            dar_filenames,
            kicked_participant,
            new_participant,
            new_threshold: self.new_threshold.map(|v| v as i32),
            previous_threshold: self.previous_threshold.map(|v| v as i32),
            dec_party_id,
            package_names,
            workflow_instance: self.workflow_instance,
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

#[derive(Debug, sqlx::FromRow)]
pub struct WorkflowRunRow {
    pub instance_name: String,
    pub kind: String,
    pub role: String,
    pub status: String,
    pub current_step: String,
    pub step_index: i64,
    pub step_total: i64,
    pub config_json: String,
    pub coordinator_pubkey: Option<String>,
    pub coordinator_instance: Option<String>,
    pub expected_peers_json: String,
    pub completed_peers_json: String,
    pub dec_party_id: Option<String>,
    pub error: Option<String>,
    pub dismissed: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

fn workflow_progress_str(p: WorkflowProgress) -> &'static str {
    match p {
        WorkflowProgress::Idle => "idle",
        WorkflowProgress::InProgress => "inprogress",
        WorkflowProgress::Completed => "completed",
        WorkflowProgress::Failed => "failed",
        WorkflowProgress::Cancelled => "cancelled",
    }
}

fn parse_workflow_progress(s: &str) -> Result<WorkflowProgress> {
    Ok(match s {
        "idle" => WorkflowProgress::Idle,
        "inprogress" => WorkflowProgress::InProgress,
        "completed" => WorkflowProgress::Completed,
        "failed" => WorkflowProgress::Failed,
        "cancelled" => WorkflowProgress::Cancelled,
        other => anyhow::bail!("unknown workflow status: {other}"),
    })
}

impl WorkflowRunRow {
    /// Build a row from the in-memory domain object.
    ///
    /// # Errors
    ///
    /// Returns an error if the expected/completed peer lists fail to
    /// JSON-encode.
    pub fn from_domain(r: &WorkflowRun) -> Result<Self> {
        Ok(Self {
            instance_name: r.instance_name.clone(),
            kind: r.kind.to_string(),
            role: r.role.to_string(),
            status: workflow_progress_str(r.status).to_string(),
            current_step: r.current_step.clone(),
            step_index: r.step_index,
            step_total: r.step_total,
            config_json: r.config_json.clone(),
            coordinator_pubkey: r.coordinator_pubkey.clone(),
            coordinator_instance: r.coordinator_instance.clone(),
            expected_peers_json: serde_json::to_string(&r.expected_peers)
                .context("encode expected_peers")?,
            completed_peers_json: serde_json::to_string(&r.completed_peers)
                .context("encode completed_peers")?,
            dec_party_id: r.dec_party_id.as_ref().map(CantonId::to_string),
            error: r.error.clone(),
            dismissed: if r.dismissed { 1 } else { 0 },
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }

    /// Decode a row read from the database back into a domain object.
    ///
    /// # Errors
    ///
    /// Returns an error if `kind`, `role` or `status` is unrecognised, or if
    /// the expected/completed peer JSON fields fail to decode.
    pub fn into_domain(self) -> Result<WorkflowRun> {
        let kind = WorkflowKind::from_str(&self.kind)
            .with_context(|| format!("invalid workflow kind on {}", self.instance_name))?;
        let role = WorkflowRole::from_str(&self.role)
            .with_context(|| format!("invalid workflow role on {}", self.instance_name))?;
        let status = parse_workflow_progress(&self.status)
            .with_context(|| format!("invalid workflow status on {}", self.instance_name))?;
        let expected_peers: Vec<CantonId> = serde_json::from_str(&self.expected_peers_json)
            .with_context(|| format!("decode expected_peers on {}", self.instance_name))?;
        let completed_peers: Vec<CantonId> = serde_json::from_str(&self.completed_peers_json)
            .with_context(|| format!("decode completed_peers on {}", self.instance_name))?;
        let dec_party_id = self
            .dec_party_id
            .as_deref()
            .map(CantonId::parse)
            .transpose()
            .with_context(|| format!("decode dec_party_id on {}", self.instance_name))?;
        Ok(WorkflowRun {
            instance_name: self.instance_name,
            kind,
            role,
            status,
            current_step: self.current_step,
            step_index: self.step_index,
            step_total: self.step_total,
            config_json: self.config_json,
            coordinator_pubkey: self.coordinator_pubkey,
            coordinator_instance: self.coordinator_instance,
            coordinator_name: None,
            expected_peers,
            completed_peers,
            dec_party_id,
            // `prefix` + `participants` + thresholds + package/dar names are
            // derived from `config_json` at the API layer; the DB doesn't store
            // them as columns.
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            added_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: self.error,
            dismissed: self.dismissed != 0,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert a `Result` is `Err`. Test-only helper that avoids `.unwrap_err()`
    /// (forbidden by the Bitsafe standard) while keeping the call sites terse.
    fn assert_is_err<T: std::fmt::Debug>(result: Result<T>) {
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    fn base_pending_invitation_row() -> PendingInvitationRow {
        PendingInvitationRow {
            id: "inv-1".to_string(),
            invitation_type: "Onboarding".to_string(),
            coordinator_pubkey: "deadbeef".to_string(),
            received_at: 0,
            prefix: None,
            participants: None,
            dar_filenames: None,
            kicked_participant: None,
            new_participant: None,
            new_threshold: None,
            previous_threshold: None,
            dec_party_id: None,
            package_names: None,
            workflow_instance: None,
        }
    }

    #[test]
    fn pending_invitation_unknown_type_errs() {
        let row = PendingInvitationRow {
            invitation_type: "Bogus".to_string(),
            ..base_pending_invitation_row()
        };

        assert_is_err(row.into_domain());
    }

    #[test]
    fn pending_invitation_bad_participants_json_errs() {
        let row = PendingInvitationRow {
            participants: Some("not json".to_string()),
            ..base_pending_invitation_row()
        };

        assert_is_err(row.into_domain());
    }

    #[test]
    fn pending_invitation_bad_kicked_participant_errs() {
        let row = PendingInvitationRow {
            kicked_participant: Some("not-a-canton-id".to_string()),
            ..base_pending_invitation_row()
        };

        assert_is_err(row.into_domain());
    }

    #[test]
    fn pending_invitation_valid_row_decodes() -> Result {
        let row = base_pending_invitation_row();

        let inv = row.into_domain()?;

        assert_eq!(inv.id, "inv-1");
        assert!(matches!(inv.invitation_type, InvitationType::Onboarding));

        Ok(())
    }

    fn base_workflow_run_row() -> WorkflowRunRow {
        WorkflowRunRow {
            instance_name: "run-1".to_string(),
            kind: "Onboarding".to_string(),
            role: "Coordinator".to_string(),
            status: "idle".to_string(),
            current_step: "start".to_string(),
            step_index: 0,
            step_total: 1,
            config_json: "{}".to_string(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            expected_peers_json: "[]".to_string(),
            completed_peers_json: "[]".to_string(),
            dec_party_id: None,
            error: None,
            dismissed: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn workflow_run_bad_kind_errs() {
        let row = WorkflowRunRow {
            kind: "Bogus".to_string(),
            ..base_workflow_run_row()
        };

        assert_is_err(row.into_domain());
    }

    #[test]
    fn workflow_run_bad_role_errs() {
        let row = WorkflowRunRow {
            role: "Bogus".to_string(),
            ..base_workflow_run_row()
        };

        assert_is_err(row.into_domain());
    }

    #[test]
    fn workflow_run_bad_status_errs() {
        let row = WorkflowRunRow {
            status: "Bogus".to_string(),
            ..base_workflow_run_row()
        };

        assert_is_err(row.into_domain());
    }

    #[test]
    fn workflow_run_bad_expected_peers_json_errs() {
        let row = WorkflowRunRow {
            expected_peers_json: "not json".to_string(),
            ..base_workflow_run_row()
        };

        assert_is_err(row.into_domain());
    }

    #[test]
    fn workflow_run_valid_row_decodes() -> Result {
        let row = base_workflow_run_row();

        let run = row.into_domain()?;

        assert_eq!(run.instance_name, "run-1");
        assert!(matches!(run.kind, WorkflowKind::Onboarding));
        assert!(matches!(run.role, WorkflowRole::Coordinator));
        assert!(matches!(run.status, WorkflowProgress::Idle));

        Ok(())
    }

    fn base_party_credentials_row() -> PartyCredentialsRow {
        // `into_domain` parses these as full CantonIds, so they need a valid
        // 34-byte (68 hex char) namespace — unlike PARTY_A/PARTY_B above, which
        // the other tests use as raw strings they never parse.
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        PartyCredentialsRow {
            dec_party_id: format!("dec::{ns}"),
            member_party_id: format!("member::{ns}"),
            user_id: "user-1".to_string(),
            keycloak_url: "https://kc.example".to_string(),
            keycloak_realm: "realm".to_string(),
            keycloak_client_id: "kc-client".to_string(),
            keycloak_client_secret: None,
            keycloak_username: None,
            keycloak_password: None,
            auth0_domain: None,
            auth0_audience: None,
            auth0_client_id: None,
            auth0_client_secret: None,
        }
    }

    #[test]
    fn party_credentials_full_auth0_reconstructs() -> Result {
        // All four auth0 columns Some → the tuple match arm builds an
        // Auth0M2MConfig. Crypto key is unset in unit tests, so encrypt/decrypt
        // are pass-through and the plaintext values round-trip verbatim.
        let row = PartyCredentialsRow {
            auth0_domain: Some("tenant.us.auth0.com".to_string()),
            auth0_audience: Some("https://api.example".to_string()),
            auth0_client_id: Some("auth0-client".to_string()),
            auth0_client_secret: Some("auth0-secret".to_string()),
            ..base_party_credentials_row()
        };

        let creds = row.into_domain()?;

        let auth0 = match creds.auth0 {
            Some(a) => a,
            None => panic!("expected auth0 to be Some for a full config"),
        };
        assert_eq!(auth0.domain, "tenant.us.auth0.com");
        assert_eq!(auth0.audience, "https://api.example");
        assert_eq!(auth0.client_id, "auth0-client");
        assert_eq!(auth0.client_secret, "auth0-secret");

        Ok(())
    }

    #[test]
    fn party_credentials_partial_auth0_is_none() -> Result {
        // One auth0 column missing → the `_ => None` arm drops the whole config.
        let row = PartyCredentialsRow {
            auth0_domain: Some("tenant.us.auth0.com".to_string()),
            auth0_audience: Some("https://api.example".to_string()),
            auth0_client_id: None,
            auth0_client_secret: Some("auth0-secret".to_string()),
            ..base_party_credentials_row()
        };

        let creds = row.into_domain()?;

        assert!(creds.auth0.is_none());

        Ok(())
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct WorkflowArtifactRow {
    pub instance_name: String,
    pub artifact_kind: String,
    pub peer_id: String,
    pub payload: Vec<u8>,
    pub created_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DecPartyIdentityRow {
    pub dec_party_id: String,
    pub artifact_kind: String,
    pub peer_id: String,
    pub payload: Vec<u8>,
    pub created_at: i64,
}
