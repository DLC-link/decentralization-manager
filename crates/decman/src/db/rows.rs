use std::{collections::BTreeMap, str::FromStr};

use anyhow::Context;

use crate::{
    canton_id::CantonId,
    config::{
        Auth0M2MConfig, CredentialKind, KeycloakConfig, PackageConfig, PartyCredentials, Peer,
    },
    db::crypto,
    error::Result,
    server::{
        InvitationType, MemberVariant, PendingInvitation, WorkflowKind, WorkflowProgress,
        WorkflowRole, WorkflowRun,
    },
};

/// One `peers` row: the address book of design D2.
#[derive(Debug, sqlx::FromRow)]
pub struct PeerRow {
    pub participant_id: String,
    pub name: String,
    /// The peer's node party. Nullable because an operator can add a peer
    /// before that peer has a node identity; such a peer cannot be invited.
    pub party: Option<String>,
}

impl PeerRow {
    pub fn from_domain(peer: &Peer) -> Self {
        Self {
            participant_id: peer.participant_id.to_string(),
            name: peer.name.clone(),
            party: peer.party.as_ref().map(CantonId::to_string),
        }
    }

    pub fn into_domain(self) -> Result<Peer> {
        let party = self
            .party
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(CantonId::parse)
            .transpose()
            .with_context(|| format!("peers row {}: invalid party", self.participant_id))?;
        Ok(Peer {
            participant_id: CantonId::parse(&self.participant_id)?,
            name: self.name,
            party,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct PartyCredentialsRow {
    /// `decparty` or `node`; see [`CredentialKind`].
    pub kind: String,
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
            kind: creds.kind.as_str().to_string(),
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
            kind: CredentialKind::from_str(&self.kind)
                .with_context(|| format!("party_credentials row {}", self.dec_party_id))?,
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

/// What this node decided about one `WorkflowProposal`.
///
/// The row is the idempotency guard of the observer: a proposal with a
/// decision is never projected into `pending_invitations` again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalDecision {
    /// The operator accepted. The node co-signs matching topology proposals.
    Accepted,
    /// The operator declined. `WorkflowProposal_Decline` was exercised.
    Declined,
    /// The operator dismissed the card without answering.
    Dismissed,
}

impl ProposalDecision {
    /// The stored column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Dismissed => "dismissed",
        }
    }
}

impl std::fmt::Display for ProposalDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProposalDecision {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "accepted" => Ok(Self::Accepted),
            "declined" => Ok(Self::Declined),
            "dismissed" => Ok(Self::Dismissed),
            other => Err(anyhow::anyhow!("unknown proposal decision: {other}")),
        }
    }
}

/// One `proposal_decisions` row, decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalDecisionEntry {
    /// The `WorkflowProposal` contract id the decision is about.
    pub proposal_cid: String,
    pub decision: ProposalDecision,
    /// Unix seconds.
    pub decided_at: i64,
    /// Topology transaction hashes (Canton hex) this node agreed to co-sign.
    /// Empty until the observer pins them.
    pub pinned_hashes: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ProposalDecisionRow {
    pub proposal_cid: String,
    pub decision: String,
    pub decided_at: i64,
    pub pinned_hashes_json: Option<String>,
}

impl ProposalDecisionRow {
    pub fn from_domain(entry: &ProposalDecisionEntry) -> Result<Self> {
        Ok(Self {
            proposal_cid: entry.proposal_cid.clone(),
            decision: entry.decision.as_str().to_string(),
            decided_at: entry.decided_at,
            pinned_hashes_json: encode_list(&entry.pinned_hashes, "proposal pinned hashes")?,
        })
    }

    pub fn into_domain(self) -> Result<ProposalDecisionEntry> {
        let decision = ProposalDecision::from_str(&self.decision)
            .with_context(|| format!("proposal_decisions row {}", self.proposal_cid))?;
        let pinned_hashes =
            decode_list(self.pinned_hashes_json, &self.proposal_cid, "pinned_hashes")?;
        Ok(ProposalDecisionEntry {
            proposal_cid: self.proposal_cid,
            decision,
            decided_at: self.decided_at,
            pinned_hashes,
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
    /// Fingerprint of the Daml signing key this participant contributes to
    /// the party's `party_signing_keys`. `None` until the participant
    /// reports it in a `WorkflowAcceptance` (design D6).
    pub signing_key: Option<String>,
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

/// One `pending_invitations` row: the local cache of an unaccepted
/// `WorkflowProposal` that names this node (design D11). `id` is the
/// proposal contract id.
#[derive(Debug, sqlx::FromRow)]
pub struct PendingInvitationRow {
    pub id: String,
    pub invitation_type: String,
    pub coordinator_participant: String,
    pub coordinator_party: Option<String>,
    pub proposal_cid: Option<String>,
    pub expires_at: Option<i64>,
    pub received_at: i64,
    pub prefix: Option<String>,
    pub participants: Option<String>,
    pub dar_filenames: Option<String>,
    pub dar_hashes: Option<String>,
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
            coordinator_participant: inv.coordinator_participant.clone(),
            coordinator_party: inv.coordinator_party.as_ref().map(CantonId::to_string),
            proposal_cid: Some(inv.proposal_cid.clone()),
            expires_at: inv.expires_at,
            received_at: inv.received_at,
            prefix: inv.prefix.clone(),
            participants: encode_list(&inv.participants, "pending invitation participants")?,
            dar_filenames: encode_list(&inv.dar_filenames, "pending invitation dar_filenames")?,
            dar_hashes: encode_list(&inv.dar_hashes, "pending invitation dar_hashes")?,
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
        let dar_hashes = decode_list(self.dar_hashes, &self.id, "dar_hashes")?;
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
        let coordinator_party = self
            .coordinator_party
            .map(|s| CantonId::parse(&s))
            .transpose()
            .with_context(|| format!("invalid coordinator_party for id {}", self.id))?;
        // The card id is the proposal contract id, so a row without the
        // column filled still names its proposal.
        let proposal_cid = self.proposal_cid.unwrap_or_else(|| self.id.clone());
        Ok(PendingInvitation {
            id: self.id,
            invitation_type,
            coordinator_participant: self.coordinator_participant,
            coordinator_party,
            proposal_cid,
            coordinator_name: None,
            received_at: self.received_at,
            expires_at: self.expires_at,
            prefix: self.prefix,
            participants,
            dar_filenames,
            dar_hashes,
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

/// One `workflow_runs` row: the UI projection of a run (design D11). The
/// on-ledger fields (`proposal_cid`, `coordinator_party`,
/// `coordinator_participant`, `member_variant`, `topology_hashes_json`) are
/// what the observer drives a run from; a row without a `proposal_cid`
/// is a legacy row that predates the 2.0 upgrade and is only displayed.
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
    pub coordinator_participant: Option<String>,
    pub coordinator_party: Option<String>,
    pub proposal_cid: Option<String>,
    pub topology_hashes_json: Option<String>,
    pub member_variant: Option<String>,
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
            coordinator_participant: r.coordinator_participant.clone(),
            coordinator_party: r.coordinator_party.as_ref().map(CantonId::to_string),
            proposal_cid: r.proposal_cid.clone(),
            topology_hashes_json: if r.topology_hashes.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&r.topology_hashes).context("encode topology_hashes")?)
            },
            member_variant: r.member_variant.map(|v| v.as_str().to_string()),
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
        let coordinator_party = self
            .coordinator_party
            .as_deref()
            .map(CantonId::parse)
            .transpose()
            .with_context(|| format!("decode coordinator_party on {}", self.instance_name))?;
        let topology_hashes: BTreeMap<String, String> = match self.topology_hashes_json.as_deref() {
            Some(json) if !json.is_empty() => serde_json::from_str(json)
                .with_context(|| format!("decode topology_hashes on {}", self.instance_name))?,
            _ => BTreeMap::new(),
        };
        let member_variant = self
            .member_variant
            .as_deref()
            .filter(|v| !v.is_empty())
            .map(MemberVariant::from_str)
            .transpose()
            .with_context(|| format!("decode member_variant on {}", self.instance_name))?;
        Ok(WorkflowRun {
            instance_name: self.instance_name,
            kind,
            role,
            status,
            current_step: self.current_step,
            step_index: self.step_index,
            step_total: self.step_total,
            config_json: self.config_json,
            coordinator_participant: self.coordinator_participant,
            coordinator_party,
            proposal_cid: self.proposal_cid,
            member_variant,
            topology_hashes,
            coordinator_instance: self.coordinator_instance,
            coordinator_name: None,
            expected_peers,
            completed_peers,
            connected_peers: Vec::new(),
            acs_progress: None,
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
            id: "00inv-1".to_string(),
            invitation_type: "Onboarding".to_string(),
            coordinator_participant: "participant1::1220aa".to_string(),
            coordinator_party: None,
            proposal_cid: None,
            expires_at: None,
            received_at: 0,
            prefix: None,
            participants: None,
            dar_filenames: None,
            dar_hashes: None,
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

        assert_eq!(inv.id, "00inv-1");
        assert!(matches!(inv.invitation_type, InvitationType::Onboarding));
        // A row without the column still names its proposal through `id`.
        assert_eq!(inv.proposal_cid, "00inv-1");
        assert!(inv.coordinator_party.is_none());

        Ok(())
    }

    #[test]
    fn pending_invitation_bad_coordinator_party_errs() {
        let row = PendingInvitationRow {
            coordinator_party: Some("not-a-canton-id".to_string()),
            ..base_pending_invitation_row()
        };

        assert_is_err(row.into_domain());
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
            coordinator_participant: None,
            coordinator_party: None,
            proposal_cid: None,
            topology_hashes_json: None,
            member_variant: None,
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
        assert!(run.proposal_cid.is_none());
        assert!(run.topology_hashes.is_empty());
        assert!(run.member_variant.is_none());

        Ok(())
    }

    /// The on-ledger columns round-trip, and an empty hash map is stored as
    /// NULL so a row the observer never touched stays byte-identical.
    #[test]
    fn workflow_run_onledger_columns_round_trip() -> Result {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let mut run = base_workflow_run_row().into_domain()?;
        run.coordinator_participant = Some(format!("participant1::{ns}"));
        run.coordinator_party = Some(CantonId::parse(&format!("node-a::{ns}"))?);
        run.proposal_cid = Some("00proposal".to_string());
        run.member_variant = Some(MemberVariant::Joiner);
        run.topology_hashes
            .insert("dnd".to_string(), "1220ab".to_string());

        let row = WorkflowRunRow::from_domain(&run)?;
        assert_eq!(row.member_variant.as_deref(), Some("Joiner"));
        assert_eq!(
            row.topology_hashes_json.as_deref(),
            Some(r#"{"dnd":"1220ab"}"#)
        );
        let back = row.into_domain()?;
        assert_eq!(back.coordinator_party, run.coordinator_party);
        assert_eq!(back.proposal_cid.as_deref(), Some("00proposal"));
        assert_eq!(back.member_variant, Some(MemberVariant::Joiner));
        assert_eq!(back.topology_hashes, run.topology_hashes);

        let empty = WorkflowRunRow::from_domain(&base_workflow_run_row().into_domain()?)?;
        assert!(empty.topology_hashes_json.is_none());
        assert!(empty.member_variant.is_none());
        Ok(())
    }

    #[test]
    fn workflow_run_bad_member_variant_errs() {
        let row = WorkflowRunRow {
            member_variant: Some("Bystander".to_string()),
            ..base_workflow_run_row()
        };

        assert_is_err(row.into_domain());
    }

    fn base_party_credentials_row() -> PartyCredentialsRow {
        // `into_domain` parses these as full CantonIds, so they need a valid
        // 34-byte (68 hex char) namespace — unlike PARTY_A/PARTY_B above, which
        // the other tests use as raw strings they never parse.
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        PartyCredentialsRow {
            kind: "decparty".to_string(),
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
