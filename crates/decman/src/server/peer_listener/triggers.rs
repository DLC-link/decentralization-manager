//! The workflow triggers the listener shares across connections: invitations, cancel and retry.

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{RwLock, mpsc};

use crate::server::state::PeerChunkCache;
use common::api::ContractDeploymentIntent;

use crate::canton_id::CantonId;
use crate::server::types::{
    AddPartyInvitePayload, ChangeThresholdInvitePayload, ContractsInvitePayload, DarsInvitePayload,
    InvitationType, KickInvitePayload, OnboardingInvitePayload, PendingInvitation,
    WorkflowProgress, WorkflowRole,
};
use crate::server::{PeerJob, WorkflowRegistry, refire_peer};
use crate::{
    config::{NodeConfig, PartyCredentials},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
};

/// Workflow triggers shared across Noise server handlers
#[derive(Clone)]
pub(crate) struct WorkflowTriggers {
    pub(crate) pending_invitations: Arc<RwLock<Vec<PendingInvitation>>>,
    /// Full node config — read by handler arms that need the participant
    /// identity, admin URL, or synchronizer alias as a bundle (e.g.
    /// `list_my_owner_keys`'s P2P filter, see #149).
    pub(crate) config: NodeConfig,
    /// Cache for chunked `RequestDar` responses, keyed by the requesting
    /// peer's pubkey (hex). Populated when a requested DAR exceeds
    /// `MAX_PAYLOAD_SIZE`; consumed by subsequent GetChunk requests from the
    /// same peer. TTL: 30 seconds. One entry per peer; replaced on the next
    /// `RequestDar` from the same peer.
    pub(crate) peer_chunk_cache: PeerChunkCache,
    pub(crate) db: SqlitePool,
    /// Read by the `RequestMemberParty` listener arm.
    pub(crate) party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    /// Every in-flight workflow this node owns, keyed by `instance_name`. The
    /// listener routes a peer's workflow-command (`Message::instance`) to the
    /// matching coordinator run via `workflows.route(..)`.
    pub(crate) workflows: WorkflowRegistry,
    /// Used by the `RetryWorkflow` listener arm to re-enqueue Failed peer runs
    /// as `PeerJob`s.
    pub(crate) peer_job_sender: mpsc::UnboundedSender<PeerJob>,
}

pub(super) enum InvitationMeta {
    None,
    Onboarding(OnboardingInvitePayload),
    Dars(DarsInvitePayload),
    Kick(KickInvitePayload),
    Contracts(ContractsInvitePayload),
    AddParty(AddPartyInvitePayload),
    ChangeThreshold(ChangeThresholdInvitePayload),
}

/// The fields of a pending invitation that its invite payload fills in.
/// What a kind does not carry stays empty.
#[derive(Default)]
struct InvitationFields {
    prefix: Option<String>,
    participants: Vec<CantonId>,
    dar_filenames: Vec<String>,
    dar_hashes: Vec<String>,
    kicked_participant: Option<CantonId>,
    kicked_owner_key: Option<String>,
    new_participant: Option<CantonId>,
    new_threshold: Option<i32>,
    previous_threshold: Option<i32>,
    confirmation_threshold: Option<i32>,
    dec_party_id: Option<CantonId>,
    package_names: Vec<String>,
    contract_intents: Vec<ContractDeploymentIntent>,
    workflow_instance: Option<String>,
}

impl From<InvitationMeta> for InvitationFields {
    fn from(meta: InvitationMeta) -> Self {
        match meta {
            InvitationMeta::None => Self::default(),
            InvitationMeta::Onboarding(p) => Self {
                prefix: Some(p.prefix),
                participants: p.participants,
                new_threshold: p.threshold,
                confirmation_threshold: p.confirmation_threshold,
                workflow_instance: p.workflow_instance,
                ..Self::default()
            },
            InvitationMeta::Dars(p) => Self {
                dar_filenames: p.dar_filenames,
                dar_hashes: p.dar_hashes,
                participants: p.participants,
                workflow_instance: p.workflow_instance,
                ..Self::default()
            },
            InvitationMeta::Kick(p) => Self {
                kicked_participant: p.kicked_participant,
                kicked_owner_key: p.kicked_owner_key,
                new_threshold: Some(p.new_threshold),
                previous_threshold: Some(p.previous_threshold),
                confirmation_threshold: p.confirmation_threshold,
                dec_party_id: Some(p.dec_party_id),
                participants: p.participants,
                workflow_instance: p.workflow_instance,
                ..Self::default()
            },
            InvitationMeta::Contracts(p) => Self {
                dec_party_id: Some(p.dec_party_id),
                participants: p.participants,
                package_names: p.package_names,
                contract_intents: p.contract_intents,
                workflow_instance: p.workflow_instance,
                ..Self::default()
            },
            InvitationMeta::AddParty(p) => Self {
                new_participant: Some(p.new_participant),
                new_threshold: Some(p.new_threshold),
                previous_threshold: Some(p.previous_threshold),
                confirmation_threshold: p.confirmation_threshold,
                dec_party_id: Some(p.dec_party_id),
                participants: p.participants,
                workflow_instance: p.workflow_instance,
                ..Self::default()
            },
            InvitationMeta::ChangeThreshold(p) => Self {
                new_threshold: Some(p.new_threshold),
                previous_threshold: Some(p.previous_threshold),
                confirmation_threshold: p.confirmation_threshold,
                dec_party_id: Some(p.dec_party_id),
                participants: p.participants,
                workflow_instance: p.workflow_instance,
                ..Self::default()
            },
        }
    }
}

impl WorkflowTriggers {
    pub(super) async fn record_invitation(
        &self,
        invitation_type: InvitationType,
        coordinator_pubkey: &str,
        meta: InvitationMeta,
    ) {
        let InvitationFields {
            prefix,
            participants,
            dar_filenames,
            dar_hashes,
            kicked_participant,
            kicked_owner_key,
            new_participant,
            new_threshold,
            previous_threshold,
            confirmation_threshold,
            dec_party_id,
            package_names,
            contract_intents,
            workflow_instance,
        } = meta.into();
        // Key the id on the coordinator's run instance when available so a
        // NEW run's invite never silently morphs an older card in place — an
        // accept/decline racing the replacement then misses (404) instead of
        // acting on an invitation the user never saw. Re-sends of the SAME
        // run still dedup (same instance → same id). Invites from old
        // coordinators (no instance) keep the legacy type+pubkey id.
        let type_str = invitation_type.as_str().to_lowercase();
        let pubkey_short = &coordinator_pubkey[..16];
        let id = match &workflow_instance {
            Some(instance) => format!("{type_str}-{pubkey_short}-{instance}"),
            None => format!("{type_str}-{pubkey_short}"),
        };
        let invitation = PendingInvitation {
            id,
            invitation_type,
            coordinator_pubkey: coordinator_pubkey.to_string(),
            coordinator_name: None,
            received_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            prefix,
            participants,
            dar_filenames,
            dar_hashes,
            kicked_participant,
            kicked_owner_key,
            new_participant,
            new_threshold,
            previous_threshold,
            confirmation_threshold,
            dec_party_id,
            package_names,
            contract_intents,
            workflow_instance,
        };

        // Dedup by `id` (which includes the coordinator's run `instance`), not
        // by (type, coordinator): a coordinator can now run several workflows
        // of the same kind concurrently, so each distinct run must surface as
        // its own card. Only a re-send of the SAME run (same id) replaces the
        // existing card; `upsert_pending_invitation` is keyed on `id`, so the
        // upsert alone dedups same-run re-sends without dropping sibling runs.
        match self.db.begin_transaction().await {
            Ok(mut tx) => {
                if let Err(e) = tx.upsert_pending_invitation(&invitation).await {
                    tracing::warn!("Failed to persist pending invitation: {e}");
                } else if let Err(e) = Commitable::commit(tx).await {
                    tracing::warn!("Failed to commit pending invitation: {e}");
                }
            }
            Err(e) => tracing::warn!("Failed to begin tx for pending invitation: {e}"),
        }

        let mut invitations = self.pending_invitations.write().await;
        invitations.retain(|i| i.id != invitation.id);
        invitations.push(invitation);

        // Bound the per-coordinator backlog: invites are recorded
        // unconditionally now (no busy-gating), so a buggy or hostile —
        // though authenticated — peer could otherwise grow
        // pending_invitations without limit by inventing fresh instances.
        // Keep the newest MAX_PENDING_INVITES_PER_COORDINATOR per sender,
        // evicting the oldest.
        const MAX_PENDING_INVITES_PER_COORDINATOR: usize = 16;
        let coordinator = invitations
            .last()
            .map(|i| i.coordinator_pubkey.clone())
            .unwrap_or_default();
        let mut from_sender: Vec<(i64, String)> = invitations
            .iter()
            .filter(|i| i.coordinator_pubkey == coordinator)
            .map(|i| (i.received_at, i.id.clone()))
            .collect();
        if from_sender.len() > MAX_PENDING_INVITES_PER_COORDINATOR {
            from_sender.sort_by_key(|(at, _)| *at);
            let evict: Vec<String> = from_sender
                [..from_sender.len() - MAX_PENDING_INVITES_PER_COORDINATOR]
                .iter()
                .map(|(_, id)| id.clone())
                .collect();
            tracing::warn!(
                "Pending-invitation cap hit for coordinator {coordinator}: evicting {} \
                 oldest invite(s)",
                evict.len()
            );
            invitations.retain(|i| !evict.contains(&i.id));
            drop(invitations);
            match self.db.begin_transaction().await {
                Ok(mut tx) => {
                    let mut ok = true;
                    for id in &evict {
                        if let Err(e) = tx.delete_pending_invitation(id).await {
                            tracing::warn!("Failed to delete evicted invitation {id}: {e}");
                            ok = false;
                            break;
                        }
                    }
                    if ok && let Err(e) = Commitable::commit(tx).await {
                        tracing::warn!("Failed to commit invitation eviction: {e}");
                    }
                }
                Err(e) => tracing::warn!("Failed to begin tx for invitation eviction: {e}"),
            }
        }
    }

    /// Drop pending invitations from `coordinator_pubkey`. When `instance` is
    /// non-empty (the CancelInvite was stamped with the cancelled run's
    /// `instance_name`), drop only the invitation(s) for that run so a sibling
    /// concurrent run's invite survives. An empty `instance` (legacy
    /// coordinator) keeps the old drop-everything-from-sender behaviour.
    pub(super) async fn drop_invitations_from(&self, coordinator_pubkey: &str, instance: &str) {
        let mut invitations = self.pending_invitations.write().await;
        let matches = |i: &PendingInvitation| {
            i.coordinator_pubkey == coordinator_pubkey
                && (instance.is_empty() || i.workflow_instance.as_deref() == Some(instance))
        };
        let dropped_ids: Vec<String> = invitations
            .iter()
            .filter(|i| matches(i))
            .map(|i| i.id.clone())
            .collect();
        invitations.retain(|i| !matches(i));
        drop(invitations);

        match self.db.begin_transaction().await {
            Ok(mut tx) => {
                let result = if instance.is_empty() {
                    tx.delete_pending_invitations_by_coordinator(coordinator_pubkey)
                        .await
                } else {
                    let mut res = Ok(());
                    for id in &dropped_ids {
                        if let Err(e) = tx.delete_pending_invitation(id).await {
                            res = Err(e);
                            break;
                        }
                    }
                    res
                };
                if let Err(e) = result {
                    tracing::warn!("Failed to delete persisted invitations: {e}");
                } else if let Err(e) = Commitable::commit(tx).await {
                    tracing::warn!("Failed to commit invitation deletion: {e}");
                }
            }
            Err(e) => tracing::warn!("Failed to begin tx for invitation deletion: {e}"),
        }
    }

    /// Cancel peer-side workflow_runs whose coordinator matches the sender of
    /// a CancelInvite. Same authority — the coordinator who started the
    /// workflow is also the one who's allowed to abort it. Used by the
    /// CancelInvite listener arm so a single message covers both un-accepted
    /// invites AND accepted-but-running runs. When `instance` is non-empty,
    /// only the run(s) belonging to that coordinator run are cancelled —
    /// sibling concurrent runs from the same coordinator keep going; empty
    /// (legacy sender) cancels everything from the sender as before.
    pub(super) async fn cancel_peer_runs_from(&self, coordinator_pubkey: &str, instance: &str) {
        let runs = match SchemaRead::get_in_progress_workflow_runs(&self.db).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("cancel_peer_runs_from: load failed: {e}");
                return;
            }
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        for run in runs.into_iter().filter(|r| {
            r.role == WorkflowRole::Peer
                && r.coordinator_pubkey.as_deref() == Some(coordinator_pubkey)
                && (instance.is_empty() || r.coordinator_instance.as_deref() == Some(instance))
        }) {
            let mut tx = match self.db.begin_transaction().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("cancel_peer_runs_from: begin_transaction: {e}");
                    continue;
                }
            };
            if let Err(e) = tx
                .set_workflow_run_status(
                    &run.instance_name,
                    WorkflowProgress::Cancelled,
                    Some("Coordinator cancelled the workflow"),
                    now,
                )
                .await
            {
                tracing::warn!(
                    "cancel_peer_runs_from: update failed for {}: {e}",
                    run.instance_name
                );
                continue;
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::warn!(
                    "cancel_peer_runs_from: commit failed for {}: {e}",
                    run.instance_name
                );
            } else {
                tracing::info!(
                    "Cancelled peer workflow run {} (coordinator cancelled)",
                    run.instance_name
                );
            }
        }
    }

    /// Coordinator-initiated retry: find Failed peer rows whose
    /// `coordinator_pubkey` matches the sender, flip them back to InProgress,
    /// and re-enqueue their peer jobs. Same authority model as
    /// `cancel_peer_runs_from` — the coordinator who started the run is also
    /// the one allowed to retry it. When `instance` is non-empty, only the
    /// run(s) belonging to that coordinator run are retried; empty (legacy
    /// sender) retries every Failed run from the sender as before.
    pub(super) async fn retry_peer_runs_from(&self, coordinator_pubkey: &str, instance: &str) {
        let runs = match SchemaRead::get_visible_workflow_runs(&self.db).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("retry_peer_runs_from: load failed: {e}");
                return;
            }
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        for run in runs.into_iter().filter(|r| {
            r.role == WorkflowRole::Peer
                && r.status == WorkflowProgress::Failed
                && r.coordinator_pubkey.as_deref() == Some(coordinator_pubkey)
                && (instance.is_empty() || r.coordinator_instance.as_deref() == Some(instance))
        }) {
            let mut tx = match self.db.begin_transaction().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("retry_peer_runs_from: begin_transaction: {e}");
                    continue;
                }
            };
            if let Err(e) = tx
                .set_workflow_run_status(
                    &run.instance_name,
                    WorkflowProgress::InProgress,
                    None,
                    now,
                )
                .await
            {
                tracing::warn!(
                    "retry_peer_runs_from: status flip failed for {}: {e}",
                    run.instance_name
                );
                continue;
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::warn!(
                    "retry_peer_runs_from: commit failed for {}: {e}",
                    run.instance_name
                );
                continue;
            }
            refire_peer(&run, &self.peer_job_sender);
            tracing::info!(
                "Re-fired peer workflow run {} (coordinator retried)",
                run.instance_name
            );
        }
    }
}

#[cfg(test)]
mod record_invitation_tests {
    //! Pins the pending invitation each invite kind records: every field the
    //! card shows, in memory and in the database.

    use std::collections::HashMap;

    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    use super::*;
    use crate::db::MIGRATOR;

    type Result<T = ()> = anyhow::Result<T>;

    const PUBKEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn cid(prefix: &str) -> String {
        format!("{prefix}::1220{}", "a".repeat(64))
    }

    fn triggers(db: SqlitePool) -> WorkflowTriggers {
        WorkflowTriggers {
            pending_invitations: Arc::new(RwLock::new(Vec::new())),
            config: NodeConfig::default(),
            peer_chunk_cache: Arc::new(Mutex::new(HashMap::new())),
            db,
            party_credentials: Arc::new(RwLock::new(Vec::new())),
            workflows: WorkflowRegistry::new(),
            peer_job_sender: mpsc::unbounded_channel().0,
        }
    }

    fn payload<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
        Ok(serde_json::from_value(value)?)
    }

    /// Record `meta` as an invitation of `invitation_type`, and return the
    /// recorded invitation as JSON without its receive time. Assert the
    /// database row reads back the same.
    async fn record(
        db: &SqlitePool,
        invitation_type: InvitationType,
        meta: InvitationMeta,
    ) -> Result<Value> {
        let triggers = triggers(db.clone());
        triggers
            .record_invitation(invitation_type, PUBKEY, meta)
            .await;
        let recorded = triggers
            .pending_invitations
            .read()
            .await
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("nothing was recorded"))?;
        let stored = db
            .get_all_pending_invitations()
            .await?
            .into_iter()
            .find(|i| i.id == recorded.id)
            .ok_or_else(|| anyhow::anyhow!("nothing was stored"))?;
        let mut recorded = serde_json::to_value(recorded)?;
        let mut stored = serde_json::to_value(stored)?;
        for invitation in [&mut recorded, &mut stored] {
            invitation
                .as_object_mut()
                .map(|fields| fields.remove("received_at"));
        }
        assert_eq!(stored, recorded);
        Ok(recorded)
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn each_kind_records_its_fields(db: SqlitePool) -> Result {
        let instance = "run-1";
        let base = |kind: &str, id: &str| {
            json!({
                "id": id,
                "invitation_type": kind,
                "coordinator_pubkey": PUBKEY,
                "coordinator_name": null,
            })
        };
        let with = |mut base: Value, fields: Value| {
            if let (Some(base), Some(fields)) = (base.as_object_mut(), fields.as_object()) {
                base.extend(fields.clone());
            }
            base
        };

        let onboarding = record(
            &db,
            InvitationType::Onboarding,
            InvitationMeta::Onboarding(payload(json!({
                "prefix": "newparty",
                "participants": [cid("p1")],
                "threshold": 2,
                "confirmation_threshold": 1,
                "workflow_instance": instance,
            }))?),
        )
        .await?;
        assert_eq!(
            onboarding,
            with(
                base("Onboarding", "onboarding-0123456789abcdef-run-1"),
                json!({
                    "prefix": "newparty",
                    "participants": [cid("p1")],
                    "new_threshold": 2,
                    "confirmation_threshold": 1,
                    "workflow_instance": instance,
                })
            )
        );

        let dars = record(
            &db,
            InvitationType::Dars,
            InvitationMeta::Dars(payload(json!({
                "dar_filenames": ["app.dar"],
                "dar_hashes": ["hash"],
                "participants": [cid("p1")],
                "workflow_instance": instance,
            }))?),
        )
        .await?;
        assert_eq!(
            dars,
            with(
                base("Dars", "dars-0123456789abcdef-run-1"),
                json!({
                    "dar_filenames": ["app.dar"],
                    "dar_hashes": ["hash"],
                    "participants": [cid("p1")],
                    "workflow_instance": instance,
                })
            )
        );

        let kick = record(
            &db,
            InvitationType::Kick,
            InvitationMeta::Kick(payload(json!({
                "dec_party_id": cid("dec"),
                "kicked_participant": cid("p2"),
                "kicked_owner_key": "k2",
                "new_threshold": 1,
                "previous_threshold": 2,
                "confirmation_threshold": 1,
                "participants": [cid("p1")],
                "workflow_instance": instance,
            }))?),
        )
        .await?;
        assert_eq!(
            kick,
            with(
                base("Kick", "kick-0123456789abcdef-run-1"),
                json!({
                    "participants": [cid("p1")],
                    "kicked_participant": cid("p2"),
                    "kicked_owner_key": "k2",
                    "new_threshold": 1,
                    "previous_threshold": 2,
                    "confirmation_threshold": 1,
                    "dec_party_id": cid("dec"),
                    "workflow_instance": instance,
                })
            )
        );

        let contracts = record(
            &db,
            InvitationType::Contracts,
            InvitationMeta::Contracts(payload(json!({
                "dec_party_id": cid("dec"),
                "participants": [cid("p1")],
                "package_names": ["Governance Core"],
                "contract_intents": [{
                    "package_id": "pkg",
                    "module_name": "Governance.Rules",
                    "entity_name": "GovernanceRules",
                    "argument_hash": "hash",
                }],
                "workflow_instance": instance,
            }))?),
        )
        .await?;
        assert_eq!(
            contracts,
            with(
                base("Contracts", "contracts-0123456789abcdef-run-1"),
                json!({
                    "participants": [cid("p1")],
                    "dec_party_id": cid("dec"),
                    "package_names": ["Governance Core"],
                    "contract_intents": [{
                        "package_id": "pkg",
                        "module_name": "Governance.Rules",
                        "entity_name": "GovernanceRules",
                        "argument_hash": "hash",
                    }],
                    "workflow_instance": instance,
                })
            )
        );

        let add_party = record(
            &db,
            InvitationType::AddParty,
            InvitationMeta::AddParty(payload(json!({
                "dec_party_id": cid("dec"),
                "new_participant": cid("p4"),
                "new_threshold": 2,
                "previous_threshold": 1,
                "confirmation_threshold": 2,
                "participants": [cid("p1")],
                "workflow_instance": instance,
            }))?),
        )
        .await?;
        assert_eq!(
            add_party,
            with(
                base("AddParty", "addparty-0123456789abcdef-run-1"),
                json!({
                    "participants": [cid("p1")],
                    "new_threshold": 2,
                    "previous_threshold": 1,
                    "confirmation_threshold": 2,
                    "dec_party_id": cid("dec"),
                    "new_participant": cid("p4"),
                    "workflow_instance": instance,
                })
            )
        );

        let change_threshold = record(
            &db,
            InvitationType::ChangeThreshold,
            InvitationMeta::ChangeThreshold(payload(json!({
                "dec_party_id": cid("dec"),
                "new_threshold": 3,
                "previous_threshold": 2,
                "confirmation_threshold": 2,
                "participants": [cid("p1")],
                "workflow_instance": instance,
            }))?),
        )
        .await?;
        assert_eq!(
            change_threshold,
            with(
                base("ChangeThreshold", "changethreshold-0123456789abcdef-run-1"),
                json!({
                    "participants": [cid("p1")],
                    "new_threshold": 3,
                    "previous_threshold": 2,
                    "confirmation_threshold": 2,
                    "dec_party_id": cid("dec"),
                    "workflow_instance": instance,
                })
            )
        );

        // An invite whose payload did not decode records only its kind, under
        // the legacy id.
        let bare = record(&db, InvitationType::Kick, InvitationMeta::None).await?;
        assert_eq!(bare, base("Kick", "kick-0123456789abcdef"));
        Ok(())
    }
}
