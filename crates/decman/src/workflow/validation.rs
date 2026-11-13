//! Peer-side validation of coordinator payloads against the accepted invitation.
//!
//! A peer authenticates the coordinator over the Noise channel and its
//! operator accepts one invitation per workflow run. Everything after that is
//! automatic, so without this module the peer signs whatever the coordinator
//! sends: a topology transaction for any party, with any owner set and any
//! threshold, or a DAR it never agreed to install.
//!
//! The invitation the operator accepted is the peer's statement of intent, and
//! it is persisted on the peer's `workflow_runs` row. [`PeerExpectations`]
//! reads it back and every signing step checks its payload against it. A
//! mismatch is refused — the peer never signs something the operator did not
//! agree to.
//!
//! Checks that the invitation can already answer are always enforced. Fields
//! added later (the DAR hashes) are enforced when the coordinator sends them
//! and warned about when it does not, so a network mid-upgrade keeps working.

use std::collections::{BTreeMap, BTreeSet};

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeyUsage, SigningPublicKey},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToKeyMapping, PartyToParticipant,
        SignedTopologyTransaction, TopologyTransaction, enums, topology_mapping,
    },
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    server::WorkflowKind,
    signing::custody,
    utils,
    workflow::{
        onboarding::steps::proposals::create::{
            compute_decentralized_namespace, decode_keys_payload,
        },
        rotate_key::config::{require_complete_inline_keys, signing_threshold_in_force},
        signing_keys::{
            ensure_survivor_keys_preserved, kick_key_claims, known_signing_keys_by_member,
            own_namespace_key, recorded_signing_keys_by_member,
        },
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
        topology, topology_version,
    },
};

/// What an unrecorded local key bundle means for a check that needs it.
///
/// Onboarding generates the bundle on the run itself, so its absence there is
/// a broken run. Every later proposal falls back to the long-lived identity
/// row, which a party onboarded before that table existed does not have — the
/// same legacy gap the DNS owner-set check tolerates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WhenUnrecorded {
    Fail,
    Skip,
}

/// What the operator agreed to when they accepted the invitation.
///
/// Built from the peer's own `workflow_runs` row, which
/// [`crate::server::handlers::invitations::insert_peer_run`] writes at accept
/// time from the invitation payload. That row — not anything the coordinator
/// sends later — is the reference every check compares against.
#[derive(Clone, Debug)]
pub struct PeerExpectations {
    /// The workflow kind the operator accepted. Commands belonging to any
    /// other kind are refused outright.
    pub kind: WorkflowKind,
    pub contract_intents: Vec<common::api::ContractDeploymentIntent>,
    /// The full member set accepted in the invitation, plus the coordinator.
    /// Topology invitations include surviving hosts that are not invited to
    /// sign; onboarding lists its invitees without the coordinator.
    pub members: BTreeSet<CantonId>,
    /// The dec party the run targets. Absent for onboarding, where the party
    /// does not exist yet.
    pub dec_party_id: Option<CantonId>,
    /// Onboarding only: the party id prefix the coordinator proposed.
    pub prefix: Option<String>,
    /// The threshold the topology should end up with.
    pub threshold: Option<i32>,
    /// The confirmation threshold the P2P mapping should end up with. `None`
    /// means it equals `threshold`, which is all an older coordinator sends.
    pub confirmation_threshold: Option<i32>,
    /// Kick only: the participant being removed.
    pub kicked_participant: Option<CantonId>,
    /// Kick only: the namespace owner key being removed. Absent from
    /// coordinators that predate the field.
    pub kicked_owner_key: Option<String>,
    /// Add-party only: the participant being added.
    pub new_participant: Option<CantonId>,
    /// Dars only: the filenames the operator agreed to install.
    pub dar_filenames: Vec<String>,
    /// Dars only: SHA-256 of each accepted DAR, index-aligned with
    /// `dar_filenames`. Empty when the coordinator predates the field.
    pub dar_hashes: Vec<String>,
    /// RotateKey only: the coordinator's Daml key that the rotation replaces.
    pub rotated_key: Option<Fingerprint>,
    /// The participant that coordinates the run.
    pub coordinator_id: CantonId,
    /// This node's own participant id.
    pub self_id: CantonId,
}

/// The invitation fields `insert_peer_run` stores on the run row.
#[derive(serde::Deserialize)]
struct PeerRunConfig {
    #[serde(default)]
    contract_intents: Vec<common::api::ContractDeploymentIntent>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    participants: Vec<CantonId>,
    #[serde(default)]
    dar_filenames: Vec<String>,
    #[serde(default)]
    dar_hashes: Vec<String>,
    /// The kicked participant, stored under the coordinator's field name.
    #[serde(default)]
    participant_id: Option<CantonId>,
    /// The kicked owner key, stored under the coordinator's field name.
    #[serde(default)]
    namespace_fingerprint: Option<String>,
    #[serde(default)]
    new_participant_id: Option<CantonId>,
    #[serde(default)]
    new_threshold: Option<i32>,
    #[serde(default)]
    confirmation_threshold: Option<i32>,
    #[serde(default)]
    rotated_key: Option<String>,
}

impl PeerExpectations {
    /// Load the accepted invitation for a peer run.
    ///
    /// # Errors
    ///
    /// Errors if the run row is missing or its stored invitation cannot be
    /// decoded. Both mean the peer cannot tell what its operator agreed to,
    /// and a peer that cannot check a payload must not sign it.
    pub async fn load(
        db: &SqlitePool,
        instance_name: &str,
        node_config: &NodeConfig,
        coordinator_id: &CantonId,
    ) -> Result<Self> {
        let run = db.get_workflow_run(instance_name).await?.ok_or_else(|| {
            anyhow::anyhow!("no workflow run {instance_name} to validate against")
        })?;

        let config: PeerRunConfig = serde_json::from_str(&run.config_json).map_err(|e| {
            anyhow::anyhow!("cannot decode the accepted invitation for {instance_name}: {e}")
        })?;

        let mut members: BTreeSet<CantonId> = config.participants.into_iter().collect();
        members.insert(coordinator_id.clone());
        let rotated_key = config
            .rotated_key
            .map(|key| key.parse::<Fingerprint>())
            .transpose()
            .map_err(|e| {
                anyhow::anyhow!("the accepted invitation names a malformed rotated key: {e:#}")
            })?;

        Ok(Self {
            kind: run.kind,
            contract_intents: config.contract_intents,
            members,
            dec_party_id: run.dec_party_id,
            prefix: config.prefix,
            threshold: config.new_threshold,
            confirmation_threshold: config.confirmation_threshold,
            kicked_participant: config.participant_id,
            kicked_owner_key: config.namespace_fingerprint,
            new_participant: config.new_participant_id,
            dar_filenames: config.dar_filenames,
            dar_hashes: config.dar_hashes,
            rotated_key,
            coordinator_id: coordinator_id.clone(),
            self_id: node_config.participant_id().clone(),
        })
    }

    /// Validate the onboarding config the coordinator sends before this node
    /// generates keys.
    ///
    /// `generate_keys` derives the vault key names from `party_id_prefix`, so
    /// an unchecked prefix lets a coordinator have this node create keys under
    /// a name the operator never accepted — or reuse the keys already sitting
    /// under another party's prefix, and authorize a namespace delegation for
    /// it. The prefix is pinned before any of that happens.
    ///
    /// # Errors
    ///
    /// Errors if the prefix differs from the accepted one, or if the accepted
    /// invitation carries none.
    pub fn check_onboarding_config(&self, party_id_prefix: &str) -> Result {
        let accepted = self.prefix.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted onboarding invitation carries no party prefix, so the \
                 coordinator's config cannot be checked against it"
            )
        })?;
        if accepted != party_id_prefix {
            anyhow::bail!(
                "coordinator asked for keys under prefix {party_id_prefix} but the accepted \
                 invitation names {accepted}"
            );
        }
        Ok(())
    }

    /// Validate the DNS proposal of an onboarding run.
    ///
    /// The party does not exist yet, so there is no party id to pin against.
    /// What the peer can pin is the owner set it was invited into: one owner
    /// per accepted member, its own namespace among them, the threshold the
    /// invitation advertised, and a decentralized namespace that really is the
    /// hash of those owners (so the coordinator cannot name an unrelated
    /// namespace and have peers authorize it).
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub async fn check_onboarding_dns(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        payload: &[u8],
    ) -> Result<String> {
        let namespace_def = decode_namespace_definition(payload)?;

        let owners: BTreeSet<String> = namespace_def.owners.iter().cloned().collect();
        if owners.len() != namespace_def.owners.len() {
            anyhow::bail!("DNS proposal repeats an owner namespace");
        }
        if owners.len() != self.members.len() {
            anyhow::bail!(
                "DNS proposal has {found} owners but the accepted invitation names \
                 {expected} members",
                found = owners.len(),
                expected = self.members.len()
            );
        }

        let own_namespace = self
            .own_namespace(config, storage, instance_name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "this node's public keys are not on the {instance_name} run, so the DNS \
                     proposal cannot be checked against them"
                )
            })?;
        if !owners.iter().any(|owner| own_namespace == *owner) {
            anyhow::bail!(
                "DNS proposal does not include this node's namespace {own_namespace} \
                 among its owners"
            );
        }

        self.check_threshold("namespace", namespace_def.threshold, owners.len())?;

        let owner_set: std::collections::HashSet<String> = owners.iter().cloned().collect();
        let computed = compute_decentralized_namespace(&owner_set);
        if computed != namespace_def.decentralized_namespace {
            anyhow::bail!(
                "DNS proposal claims namespace {claimed} but its owner set hashes to \
                 {computed}",
                claimed = namespace_def.decentralized_namespace
            );
        }

        require_new_mapping(config, payload).await?;

        tracing::info!(
            "DNS proposal matches the accepted invitation: {count} owners, threshold {threshold}",
            count = owners.len(),
            threshold = namespace_def.threshold
        );
        Ok(namespace_def.decentralized_namespace)
    }

    /// Validate the P2P proposal of an onboarding run.
    ///
    /// `dns_namespace` is the namespace this peer validated and signed in the
    /// DNS step; the party id must be built from it, so the two proposals
    /// cannot describe different parties.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub async fn check_onboarding_p2p(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        payload: &[u8],
        dns_namespace: Option<&str>,
    ) -> Result {
        let mapping = decode_party_to_participant(payload)?;
        let party = CantonId::parse(&mapping.party)?;

        match &self.prefix {
            Some(prefix) if prefix != &party.prefix => anyhow::bail!(
                "P2P proposal is for party prefix {found} but the accepted invitation \
                 named {prefix}",
                found = party.prefix
            ),
            Some(_) => {}
            // `prefix` is required on every onboarding invite, so an absent one
            // means the invite never parsed — leaving the coordinator to choose
            // the party's prefix.
            None => anyhow::bail!(
                "accepted onboarding invitation carries no party prefix, so the proposed \
                 party cannot be checked against it"
            ),
        }

        // The command gate checks the workflow kind, not the step order, so a
        // coordinator can send SignP2p before SignDns. Absent therefore means
        // "this run has not signed a DNS proposal", which the coordinator
        // controls — not "an older build did not record it". Treating it as
        // legacy let a coordinator skip DNS and have the peer sign a party
        // with any namespace whose prefix happened to match.
        let Some(namespace) = dns_namespace else {
            anyhow::bail!(
                "no DNS namespace recorded for {instance_name}: this run has not signed a \
                 DNS proposal, so the P2P proposal cannot be pinned to one"
            );
        };
        if namespace != party.namespace.to_hex() {
            anyhow::bail!(
                "P2P proposal targets namespace {found} but this node signed DNS for \
                 {namespace}",
                found = party.namespace.to_hex()
            );
        }

        self.check_p2p_membership(&mapping)?;
        self.check_onboarding_markers(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;
        self.check_own_daml_key(
            config,
            storage,
            instance_name,
            &mapping,
            WhenUnrecorded::Fail,
        )
        .await?;
        require_new_mapping(config, payload).await
    }

    /// Validate the DNS + P2P proposal pair of a kick, add-party or
    /// change-threshold run.
    ///
    /// These run against a party that already exists, so the invitation pins
    /// the party id itself: both mappings must target exactly that party, with
    /// exactly the accepted member set and the accepted threshold.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub async fn check_party_proposals(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        dns_payload: &[u8],
        p2p_payload: &[u8],
    ) -> Result {
        let dec_party_id = self.dec_party_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted {kind:?} invitation carries no dec party id, so the proposals \
                 cannot be checked against it",
                kind = self.kind
            )
        })?;

        let namespace_def = decode_namespace_definition(dns_payload)?;
        self.check_party_dns_shape(dec_party_id, &namespace_def)?;

        // Losing our namespace from the owner set would leave this node hosting
        // a party it can no longer authorize changes to. The lookup itself is
        // best-effort — a party onboarded before the identity table existed may
        // have no local record — but a namespace we *can* resolve must be there.
        // A lookup or decode failure is an error, not a legacy party: letting
        // it through would hand a coordinator this peer's signature on a
        // proposal that drops its own namespace.
        let own_namespace = self.own_namespace(config, storage, instance_name).await?;
        match &own_namespace {
            Some(own_namespace)
                if !namespace_def
                    .owners
                    .iter()
                    .any(|owner| *own_namespace == *owner) =>
            {
                anyhow::bail!(
                    "DNS proposal drops this node's namespace {own_namespace} from the owner set"
                )
            }
            Some(_) => {}
            None => tracing::warn!(
                "this node's keys are unrecorded for this party (onboarded before the \
                 identity table existed); skipping the owner-set membership check"
            ),
        }

        let mapping = decode_party_to_participant(p2p_payload)?;
        if mapping.party != dec_party_id.to_string() {
            anyhow::bail!(
                "P2P proposal is for party {found} but the accepted invitation is for \
                 {dec_party_id}",
                found = mapping.party
            );
        }
        self.check_p2p_membership(&mapping)?;
        self.check_onboarding_markers(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;
        if self.kind == WorkflowKind::Kick {
            self.check_surviving_daml_keys(storage, &mapping).await?;
        }
        // Every signer also verifies its own key against its identity or vault.
        self.check_own_daml_key(
            config,
            storage,
            instance_name,
            &mapping,
            WhenUnrecorded::Skip,
        )
        .await?;
        let new_member = self.new_member_fingerprints(storage, instance_name).await?;
        let records = if self.kind == WorkflowKind::Kick {
            Some(
                self.owner_records(storage, dec_party_id, own_namespace.as_ref())
                    .await?,
            )
        } else {
            None
        };
        let legacy_keys = self
            .check_on_chain_delta(
                config,
                dec_party_id,
                (decode_proposal_serial(dns_payload)?, &namespace_def),
                (decode_proposal_serial(p2p_payload)?, &mapping),
                new_member
                    .as_ref()
                    .map(|(namespace, daml)| (namespace.as_str(), daml.as_str())),
                records.as_ref(),
            )
            .await?;
        if let Some(legacy_keys) = legacy_keys {
            let claims =
                known_signing_keys_by_member(config, storage, dec_party_id, &legacy_keys).await?;
            let hosts: Vec<String> = mapping
                .participants
                .iter()
                .map(|p| p.participant_uid.clone())
                .collect();
            let proposed: Vec<Fingerprint> = mapping
                .party_signing_keys
                .as_ref()
                .map(|keys| keys.keys.iter().map(utils::compute_fingerprint).collect())
                .unwrap_or_default();
            let new_participant = self.new_participant.as_ref().map(ToString::to_string);
            check_legacy_claims(
                self.kind,
                &hosts,
                new_participant.as_deref(),
                &proposed,
                &claims,
            )?;
        }

        tracing::info!(
            "{kind:?} proposals match the accepted invitation for {dec_party_id}",
            kind = self.kind
        );
        Ok(())
    }

    /// Defend known survivors even when they do not contribute a signature.
    /// This reads only local records; unknown legacy records remain subject to
    /// each signer's existing identity/vault check.
    async fn check_surviving_daml_keys(
        &self,
        storage: &SqlitePool,
        mapping: &PartyToParticipant,
    ) -> Result {
        let party = self
            .dec_party_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("accepted kick names no party"))?;
        let keys = mapping
            .party_signing_keys
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("P2P proposal carries no party signing keys"))?;
        let survivors: Vec<String> = self.members.iter().map(ToString::to_string).collect();
        let removed = self
            .kicked_participant
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| self.kicked_owner_key.clone());
        let claims = recorded_signing_keys_by_member(storage, party).await?;
        let claims = kick_key_claims(removed.as_deref(), &survivors, &claims, &[])?;
        ensure_survivor_keys_preserved(&keys.keys, &survivors, &claims)
    }

    /// Validate the P2P proposal of a rotate-key run, and return the new key
    /// it adds for the coordinator.
    ///
    /// The proposal must keep the members and both on-chain thresholds, and
    /// swap exactly the key the invitation names for one new key. This
    /// node's own records must attribute the replaced key to the coordinator
    /// and every other key to the member that holds it.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation, the chain, or this
    /// node's records.
    pub async fn check_rotation_proposal(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        p2p_payload: &[u8],
    ) -> Result<Fingerprint> {
        let dec_party_id = self.dec_party_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted rotate-key invitation carries no dec party id, so the proposal \
                 cannot be checked against it"
            )
        })?;
        let rotated_key = self.rotated_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted rotate-key invitation names no key to replace, so the proposal \
                 cannot be checked against it"
            )
        })?;

        let mapping = decode_party_to_participant(p2p_payload)?;
        if mapping.party != dec_party_id.to_string() {
            anyhow::bail!(
                "P2P proposal is for party {found} but the accepted invitation is for \
                 {dec_party_id}",
                found = mapping.party
            );
        }
        self.check_p2p_membership(&mapping)?;
        self.check_onboarding_markers(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;
        self.check_own_daml_key(
            config,
            storage,
            instance_name,
            &mapping,
            WhenUnrecorded::Skip,
        )
        .await?;

        let head = SynchronizerHead {
            config,
            synchronizer_id: utils::get_synchronizer_id(config).await?,
        };
        let proposed_keys = mapping
            .party_signing_keys
            .as_ref()
            .map(|keys| keys.keys.clone())
            .unwrap_or_default();
        // A proposal already in force has no head left to compare with, but
        // this node's records still decide which key it adds.
        let candidates = check_rotation_against_head(
            &head,
            dec_party_id,
            (decode_proposal_serial(p2p_payload)?, &mapping),
            rotated_key,
        )
        .await?
        .unwrap_or_else(|| proposed_keys.clone());
        let claims =
            known_signing_keys_by_member(config, storage, dec_party_id, &candidates).await?;
        let members: Vec<String> = mapping
            .participants
            .iter()
            .map(|p| p.participant_uid.clone())
            .collect();
        let proposed: Vec<Fingerprint> = proposed_keys
            .iter()
            .map(utils::compute_fingerprint)
            .collect();
        let new_key = check_rotation_claims(
            &self.coordinator_id.to_string(),
            rotated_key,
            &members,
            &proposed,
            &claims,
        )?;

        tracing::info!(
            "Rotate-key proposal for {dec_party_id} swaps only {coordinator}'s key {rotated_key} \
             for {new_key}",
            coordinator = self.coordinator_id
        );
        Ok(new_key)
    }

    /// The checks on a party DNS proposal that need no topology read: the
    /// accepted namespace, distinct owners, and the accepted threshold.
    ///
    /// The owner count is not held to the member count. A member can own the
    /// namespace without hosting the party, so the two differ on a healthy
    /// party. [`check_on_chain_delta`](Self::check_on_chain_delta) pins the
    /// owner set instead, through the owner key a kick invitation names.
    ///
    /// A kick invitation without that key comes from a coordinator that
    /// predates it, or from one that left it out. Nothing then ties the owners
    /// the kick keeps to the members it keeps, so the proposal must keep one
    /// owner per member, as before owner keys were named.
    fn check_party_dns_shape(
        &self,
        dec_party_id: &CantonId,
        namespace_def: &DecentralizedNamespaceDefinition,
    ) -> Result {
        if namespace_def.decentralized_namespace != dec_party_id.namespace.to_hex() {
            anyhow::bail!(
                "DNS proposal targets namespace {found} but the accepted invitation is for \
                 {expected}",
                found = namespace_def.decentralized_namespace,
                expected = dec_party_id.namespace.to_hex()
            );
        }
        // Canton treats the owners as a set, so a repeated namespace would keep
        // the vector length the invitation expects while shrinking the real
        // owner set — and with it the number of distinct signatures the
        // threshold represents.
        let owners: BTreeSet<&String> = namespace_def.owners.iter().collect();
        if owners.len() != namespace_def.owners.len() {
            anyhow::bail!("DNS proposal repeats an owner namespace");
        }
        if self.kind == WorkflowKind::Kick
            && self.kicked_owner_key.is_none()
            && owners.len() != self.members.len()
        {
            anyhow::bail!(
                "DNS proposal has {found} owners but the accepted invitation names {expected} \
                 members and no owner key to remove",
                found = owners.len(),
                expected = self.members.len()
            );
        }
        self.check_threshold("namespace", namespace_def.threshold, owners.len())
    }

    /// Hold both proposals to the party's on-chain state (#420, #422).
    ///
    /// Each signer only sees its own namespace and its own Daml key, so an
    /// owner or key belonging to a member that does not sign this round could
    /// otherwise be swapped for one the coordinator controls. Reading the
    /// current definition and mapping pins everything else to what is already
    /// on-chain plus the one change the run is for. A read that fails refuses
    /// the proposal: a peer that cannot check it must not sign it.
    ///
    /// Only the new member holds the new member's keys, so only it can pin the
    /// added owner and key to them; every other peer checks that at most one
    /// was added. Its refusal blocks the P2P, which Canton makes an added host
    /// sign, but not the DNS: an added owner is authorized by its own key and
    /// the existing owners' quorum, not by the new participant.
    ///
    /// A proposal at the head serial that equals the head mapping is already
    /// effective, so signing it changes nothing and it skips the delta. Only a
    /// coordinator built before #448 sends one: it proposed with `Authorize`
    /// against the synchronizer, which puts a threshold-1 namespace in force
    /// at once. Any other proposal must sit at the next serial.
    ///
    /// `new_member` is this node's `(namespace, daml)` fingerprints when it is
    /// the new member. `records` are the owner keys this node has on record,
    /// which a kick checks the owner key the invitation names against.
    async fn check_on_chain_delta(
        &self,
        config: &NodeConfig,
        dec_party_id: &CantonId,
        dns: (u32, &DecentralizedNamespaceDefinition),
        p2p: (u32, &PartyToParticipant),
        new_member: Option<(&str, &str)>,
        records: Option<&OwnerRecords>,
    ) -> Result<Option<Vec<SigningPublicKey>>> {
        let head = SynchronizerHead {
            config,
            synchronizer_id: utils::get_synchronizer_id(config).await?,
        };
        let kicked_participant = self.kicked_participant.as_ref().map(ToString::to_string);
        check_keys_against_head(
            &head,
            self.kind,
            dec_party_id,
            dns,
            p2p,
            new_member,
            Departing {
                participant: kicked_participant.as_deref(),
                owner_key: self.kicked_owner_key.as_deref(),
                records,
            },
        )
        .await
    }

    /// This node's records of who holds the party's owner keys. Its own
    /// namespace stands in for its own host entry, which no peer reports.
    async fn owner_records(
        &self,
        storage: &SqlitePool,
        dec_party_id: &CantonId,
        own_namespace: Option<&Fingerprint>,
    ) -> Result<OwnerRecords> {
        let mut host_keys: BTreeMap<String, String> = storage
            .get_dec_party_participants(dec_party_id)
            .await?
            .into_iter()
            .filter_map(|row| row.owner_key.map(|key| (row.participant_uid, key)))
            .collect();
        if let Some(own_namespace) = own_namespace {
            host_keys.insert(self.self_id.to_string(), own_namespace.to_string());
        }
        let claims = storage
            .get_dec_party_owners(dec_party_id)
            .await?
            .into_iter()
            .filter_map(|row| row.participant_uid.map(|uid| (row.owner_key, uid)))
            .collect();
        Ok(OwnerRecords { host_keys, claims })
    }

    /// The new member's `(namespace, daml)` fingerprints, when this node is
    /// the new member. Only it holds its key bundle for the run, and it must.
    async fn new_member_fingerprints(
        &self,
        storage: &SqlitePool,
        instance_name: &str,
    ) -> Result<Option<(Fingerprint, Fingerprint)>> {
        let Some(new_member) = &self.new_participant else {
            return Ok(None);
        };
        if new_member != &self.self_id {
            return Ok(None);
        }
        let payload = storage
            .read_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&new_member.to_string()),
            )
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "this node is the new member but its public keys are not on the \
                     {instance_name} run, so the added owner and key cannot be pinned to it"
                )
            })?;
        match decode_keys_payload(&payload)?.as_slice() {
            [namespace, daml] => Ok(Some((
                utils::compute_fingerprint(namespace),
                utils::compute_fingerprint(daml),
            ))),
            keys => anyhow::bail!(
                "expected 2 public keys for new member {new_member}, found {count}",
                count = keys.len()
            ),
        }
    }

    /// Validate the add-party onboarding-flag clearing proposal: same party,
    /// same members, same threshold, and every hosting participant actually
    /// clear of the onboarding flag.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub fn check_clear_onboarding(&self, payload: &[u8]) -> Result {
        let dec_party_id = self.dec_party_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!("accepted add-party invitation carries no dec party id")
        })?;
        let mapping = decode_party_to_participant(payload)?;

        if mapping.party != dec_party_id.to_string() {
            anyhow::bail!(
                "clearing proposal is for party {found} but the accepted invitation is for \
                 {dec_party_id}",
                found = mapping.party
            );
        }
        self.check_p2p_membership(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;

        if let Some(still_onboarding) = mapping
            .participants
            .iter()
            .find(|participant| participant.onboarding.is_some())
        {
            anyhow::bail!(
                "clearing proposal still flags {participant} as onboarding",
                participant = still_onboarding.participant_uid
            );
        }
        Ok(())
    }

    /// Validate the DAR files a coordinator sends before they are uploaded and
    /// vetted on this node's participant.
    ///
    /// # Errors
    ///
    /// Errors if a file was not named in the accepted invitation, if an
    /// accepted file is missing, or if a file's content does not hash to the
    /// value the invitation pinned.
    pub fn check_dars(&self, files: &[(String, Vec<u8>)]) -> Result {
        // An invitation that named no DARs authorizes no DARs. Only the hash
        // list is treated leniently for an older coordinator — `dar_filenames`
        // has been on the invite since the workflow existed, so an empty one
        // paired with actual DAR bytes is a coordinator installing code the
        // operator never saw, not a version skew.
        if self.dar_filenames.is_empty() {
            if files.is_empty() {
                return Ok(());
            }
            anyhow::bail!(
                "coordinator sent {count} DAR(s) but the accepted invitation named none",
                count = files.len()
            );
        }

        // Both sides are compared as sets, so a repeated name would collapse and
        // mask a missing or extra file. Refuse the ambiguity rather than pick a
        // reading of it.
        let accepted: BTreeSet<&str> = self.dar_filenames.iter().map(String::as_str).collect();
        if accepted.len() != self.dar_filenames.len() {
            anyhow::bail!("accepted invitation names the same DAR twice");
        }
        let received: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        if received.len() != files.len() {
            anyhow::bail!("coordinator sent the same DAR filename twice");
        }
        if let Some(unexpected) = received.difference(&accepted).next() {
            anyhow::bail!("coordinator sent DAR {unexpected}, which the invitation did not name");
        }
        if let Some(missing) = accepted.difference(&received).next() {
            anyhow::bail!("coordinator did not send DAR {missing} named by the invitation");
        }

        // Only a wholly absent hash list means "coordinator predates the field".
        // A partial one cannot be matched up with the filenames, so treating it
        // as legacy would silently drop the content pin.
        if self.dar_hashes.is_empty() {
            tracing::warn!(
                "accepted Dars invitation carried no DAR hashes; the filenames match but \
                 the content cannot be pinned (the coordinator predates the field)"
            );
            return Ok(());
        }
        if self.dar_hashes.len() != self.dar_filenames.len() {
            anyhow::bail!(
                "accepted invitation carries {hashes} DAR hash(es) for {names} filename(s), \
                 so the content cannot be pinned",
                hashes = self.dar_hashes.len(),
                names = self.dar_filenames.len()
            );
        }

        for (filename, data) in files {
            let expected = self
                .dar_filenames
                .iter()
                .zip(&self.dar_hashes)
                .find(|(name, _)| *name == filename)
                .map(|(_, hash)| hash)
                .ok_or_else(|| anyhow::anyhow!("no accepted hash for DAR {filename}"))?;
            let actual = hex::encode(Sha256::digest(data));
            if &actual != expected {
                anyhow::bail!(
                    "DAR {filename} hashes to {actual} but the invitation pinned {expected}"
                );
            }
        }

        tracing::info!(
            "all {count} DAR(s) match the accepted invitation",
            count = files.len()
        );
        Ok(())
    }

    /// The dec party a coordinator names in a workflow config must be the one
    /// the operator accepted. Without this a peer would happily sign with its
    /// keys for a party it never agreed to act for.
    ///
    /// # Errors
    ///
    /// Errors if the party differs from the accepted one.
    pub fn check_dec_party(&self, party: &CantonId) -> Result {
        match &self.dec_party_id {
            Some(expected) if expected != party => anyhow::bail!(
                "coordinator sent a config for party {party} but the accepted invitation \
                 is for {expected}"
            ),
            Some(_) => Ok(()),
            // Every invitation kind that reaches this check carries the dec
            // party as a required field, so an absent one is not version skew —
            // it means the invite never parsed and the peer cannot say what its
            // operator agreed to. `check_party_proposals` already refuses that;
            // this refuses it too rather than accepting whatever party the
            // coordinator names.
            None => anyhow::bail!(
                "accepted {kind:?} invitation carries no dec party id, so the coordinator's \
                 config cannot be checked against it",
                kind = self.kind
            ),
        }
    }

    /// The participant an add-party run introduces must be the one named in
    /// the accepted invitation.
    ///
    /// # Errors
    ///
    /// Errors if the participant differs from the accepted one.
    pub fn check_new_participant(&self, participant: &CantonId) -> Result {
        match &self.new_participant {
            Some(expected) if expected != participant => anyhow::bail!(
                "coordinator is adding {participant} but the accepted invitation names \
                 {expected}"
            ),
            Some(_) => Ok(()),
            // `new_participant` is required on every add-party invite, so an
            // absent one means the invite never parsed — not that it predates
            // the field. Without it the coordinator would pick who joins.
            None => anyhow::bail!(
                "accepted add-party invitation names no new participant, so the coordinator's \
                 choice cannot be checked against it"
            ),
        }
    }

    /// The hosting-participant set must be exactly the accepted members, and
    /// this node must be one of them.
    fn check_p2p_membership(&self, mapping: &PartyToParticipant) -> Result {
        let mut hosts = BTreeSet::new();
        for participant in &mapping.participants {
            let id = CantonId::parse(&participant.participant_uid)?;
            if !hosts.insert(id.clone()) {
                anyhow::bail!("P2P proposal lists participant {id} twice");
            }
            // Every proposal this tool builds hosts at Confirmation. Submission
            // would let that participant submit for the party directly, so a
            // silent upgrade is a change to who can act, not a detail.
            if participant.permission != enums::ParticipantPermission::Confirmation as i32 {
                anyhow::bail!(
                    "P2P proposal hosts {id} with permission {permission}, not Confirmation",
                    permission = enums::ParticipantPermission::try_from(participant.permission)
                        .map(|p| p.as_str_name().to_string())
                        .unwrap_or_else(|_| format!("unknown ({})", participant.permission))
                );
            }
        }

        if hosts != self.members {
            let extra: Vec<String> = hosts
                .difference(&self.members)
                .map(CantonId::to_string)
                .collect();
            let missing: Vec<String> = self
                .members
                .difference(&hosts)
                .map(CantonId::to_string)
                .collect();
            anyhow::bail!(
                "P2P proposal host set does not match the accepted invitation \
                 (unexpected: {extra:?}, missing: {missing:?})"
            );
        }
        if !hosts.contains(&self.self_id) {
            anyhow::bail!(
                "P2P proposal does not host this node ({self_id})",
                self_id = self.self_id
            );
        }
        if let Some(kicked) = &self.kicked_participant
            && hosts.contains(kicked)
        {
            anyhow::bail!("P2P proposal still hosts {kicked}, the participant being kicked");
        }
        if let Some(added) = &self.new_participant
            && !hosts.contains(added)
        {
            anyhow::bail!("P2P proposal does not host {added}, the participant being added");
        }
        Ok(())
    }

    /// The onboarding marker decides who is still importing an ACS. Dropping it
    /// activates the new member before its import finishes; moving it to an
    /// existing host suspends that host instead. So exactly the accepted new
    /// participant carries it, and on a run that adds nobody, no one does.
    ///
    /// Not applied to the clearing proposal, whose purpose is to remove the
    /// marker — [`check_clear_onboarding`](Self::check_clear_onboarding)
    /// requires its absence instead.
    fn check_onboarding_markers(&self, mapping: &PartyToParticipant) -> Result {
        for participant in &mapping.participants {
            let id = CantonId::parse(&participant.participant_uid)?;
            let expected = self.new_participant.as_ref() == Some(&id);
            if participant.onboarding.is_some() != expected {
                anyhow::bail!(
                    "P2P proposal marks {id} as onboarding: {found}, but the accepted \
                     invitation calls for {expected}",
                    found = participant.onboarding.is_some()
                );
            }
        }
        Ok(())
    }

    /// A `PartyToParticipant` mapping carries two thresholds: how many hosting
    /// participants must confirm, and how many of the party's signing keys
    /// authorize a transaction for it. Both are pinned — leaving the signing
    /// threshold unchecked would let a coordinator raise a confirmation
    /// threshold the operator agreed to while quietly dropping the signing one
    /// to 1.
    ///
    /// The confirmation threshold is held to the accepted confirmation
    /// threshold, and the signing one to the accepted namespace threshold.
    ///
    /// The size of the key set is pinned with them, because a threshold only
    /// means what the operator agreed to if the set it counts over does. Two
    /// of three member keys is the accepted 2; two of a set that still holds
    /// a kicked member's key is not (#428).
    fn check_p2p_thresholds(&self, mapping: &PartyToParticipant) -> Result {
        self.check_threshold_against(
            "confirmation",
            i32::try_from(mapping.threshold)?,
            self.members.len(),
            self.confirmation_threshold.or(self.threshold),
        )?;

        // Signing a mapping with no party signing keys does not merely leave a
        // threshold unchecked — it submits a party that can no longer
        // authorize anything. Every mapping this tool produces carries them
        // (onboarding sets them, and the kick / add-party / change-threshold /
        // clearing proposals all derive from the current on-chain mapping), so
        // an absent set is a stripped proposal.
        let Some(signing_keys) = &mapping.party_signing_keys else {
            anyhow::bail!(
                "P2P proposal carries no party signing keys, which would leave the party \
                 unable to authorize anything"
            );
        };

        self.check_threshold(
            "signing",
            i32::try_from(signing_keys.threshold)?,
            self.members.len(),
        )?;

        // Every member contributes exactly one Daml key, so the counts match.
        // A peer cannot tell whose key is whose — the mapping records no owner
        // per key — but it can tell that one too many is present, which is
        // what a kick that forgot to drop the departing member's key looks
        // like from here.
        if signing_keys.keys.len() != self.members.len() {
            anyhow::bail!(
                "P2P proposal carries {keys} party signing key(s) for {members} member(s); \
                 each member contributes exactly one, so a surplus key would let a \
                 non-member keep authorizing for the party",
                keys = signing_keys.keys.len(),
                members = self.members.len()
            );
        }
        Ok(())
    }

    /// The threshold must be the one the invitation advertised, and in any
    /// case a sane value for the owner count — a threshold of 0 or one above
    /// the owner count would either need no signatures or deadlock the party.
    ///
    /// `kind` names which threshold is being checked. A mapping carries a
    /// confirmation and a signing threshold and both come through here, so a
    /// message that named neither read as though the coordinator had ignored
    /// the operator's input when in fact the other one was wrong.
    fn check_threshold(&self, kind: &str, threshold: i32, owner_count: usize) -> Result {
        self.check_threshold_against(kind, threshold, owner_count, self.threshold)
    }

    /// [`check_threshold`](Self::check_threshold) against an `expected`
    /// value other than the namespace threshold.
    fn check_threshold_against(
        &self,
        kind: &str,
        threshold: i32,
        owner_count: usize,
        expected: Option<i32>,
    ) -> Result {
        let owner_count = i32::try_from(owner_count)?;
        if !(1..=owner_count).contains(&threshold) {
            anyhow::bail!("proposed {kind} threshold {threshold} is outside 1..={owner_count}");
        }
        match expected {
            Some(expected) if expected != threshold => {
                anyhow::bail!(
                    "proposed {kind} threshold {threshold} differs from the accepted {expected}"
                )
            }
            Some(_) => Ok(()),
            // Only the onboarding invite's threshold is optional on the wire
            // (`Option<i32>`, absent from coordinators that predate it). Kick,
            // add-party and change-threshold all require it, so an absent one
            // there would let the coordinator pick any in-range value.
            None if self.kind == WorkflowKind::Onboarding => {
                tracing::warn!(
                    "accepted onboarding invitation carried no threshold; only the range \
                     check was applied to the proposed {kind} threshold {threshold}"
                );
                Ok(())
            }
            None => anyhow::bail!(
                "accepted {workflow:?} invitation carries no threshold, so the proposed \
                 {kind} threshold {threshold} cannot be checked against it",
                workflow = self.kind
            ),
        }
    }

    /// This node's Daml signing key must be among the party's signing keys, or
    /// it would be a member that cannot sign for the party it just authorized.
    async fn check_own_daml_key(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        mapping: &PartyToParticipant,
        when_unrecorded: WhenUnrecorded,
    ) -> Result {
        let keys = match self.own_keys(storage, instance_name).await? {
            Some(keys) => keys,
            None if when_unrecorded == WhenUnrecorded::Fail => anyhow::bail!(
                "this node's public keys are not on the {instance_name} run, so the P2P \
                 proposal cannot be checked against them"
            ),
            None => {
                tracing::warn!(
                    "this node's keys are unrecorded for this party (onboarded before the \
                     identity table existed); checking the proposal against the keys this \
                     node holds instead"
                );
                return self.check_holds_a_party_key(config, storage, mapping).await;
            }
        };
        let own_daml_fingerprint = utils::compute_fingerprint(&keys[1]);
        let signing_keys = mapping
            .party_signing_keys
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("P2P proposal carries no party signing keys"))?;

        if !signing_keys
            .keys
            .iter()
            .any(|key| utils::compute_fingerprint(key) == own_daml_fingerprint)
        {
            anyhow::bail!(
                "P2P proposal does not carry this node's Daml signing key \
                 {own_daml_fingerprint}"
            );
        }
        Ok(())
    }

    /// The fallback for a party this node has no local key record for: the
    /// vault and DecMan's key store still know whether this node holds the
    /// private half of one of the proposed keys.
    ///
    /// Without it a legacy party's members cannot check the attribution at
    /// all — the coordinator works out whose key is whose from its own
    /// records, and a stale one would take a member's authority away
    /// irreversibly, because inline keys override the mapping the member's
    /// key still sits in.
    ///
    /// # Errors
    ///
    /// Errors when the proposal carries no signing keys, when this node holds
    /// none of them, or when the vault or the key store cannot be read.
    async fn check_holds_a_party_key(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        mapping: &PartyToParticipant,
    ) -> Result {
        let Some(signing_keys) = mapping.party_signing_keys.as_ref() else {
            anyhow::bail!("P2P proposal carries no party signing keys");
        };
        let party = CantonId::parse(&mapping.party)?;

        for key in &signing_keys.keys {
            let fingerprint = utils::compute_fingerprint(key);
            if custody::holds_party_key(config, storage, &party, &fingerprint).await? {
                return Ok(());
            }
        }

        anyhow::bail!(
            "P2P proposal carries {count} party signing key(s) and this node holds the \
             private half of none of them, so it would be left a member that cannot sign \
             for the party",
            count = signing_keys.keys.len()
        )
    }

    /// This node's namespace fingerprint for the run: from the keys it
    /// generated during onboarding, falling back to the long-lived identity
    /// row when the run's artefacts are already gone.
    async fn own_namespace(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
    ) -> Result<Option<Fingerprint>> {
        // An identity row recovered from the chain can only carry the party's
        // Daml key, and older builds wrote a copy of it where the namespace
        // key belongs. Reading that as a namespace fingerprint would compare a
        // Daml key against the owner set and refuse every proposal, so such a
        // bundle does not answer the question.
        if let Some(keys) = self.own_keys(storage, instance_name).await?
            && let Some(fingerprint) = namespace_fingerprint(&keys)
        {
            return Ok(Some(fingerprint));
        }

        // No usable local record. For a party that already exists the vault
        // still answers it: the namespace key this node holds that the party's
        // current owner set names. Skipping the check instead would let a
        // coordinator swap this node's namespace out of the owner set with
        // every other check still passing, which signs the node's own
        // topology authority away.
        let Some(dec_party_id) = self.dec_party_id.as_ref() else {
            tracing::warn!(
                "this node's keys are unrecorded for this run and it targets no existing \
                 party, so its namespace cannot be resolved"
            );
            return Ok(None);
        };

        let synchronizer_id = utils::get_synchronizer_id(config).await?;
        let resolved = own_namespace_key(config, dec_party_id, &synchronizer_id)
            .await?
            .map(|key| utils::compute_fingerprint(&key));
        match &resolved {
            Some(fingerprint) => tracing::info!(
                "Resolved this node's namespace {fingerprint} for {dec_party_id} from the \
                 vault; no usable local key record"
            ),
            None => tracing::warn!(
                "this node holds no namespace key that {dec_party_id}'s owner set names, so \
                 the owner-set check cannot be applied"
            ),
        }
        Ok(resolved)
    }

    /// This node's `[namespace_key, daml_key]` bundle.
    async fn own_keys(
        &self,
        storage: &SqlitePool,
        instance_name: &str,
    ) -> Result<Option<Vec<SigningPublicKey>>> {
        let self_id = self.self_id.to_string();
        let stored = match storage
            .read_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&self_id),
            )
            .await?
        {
            Some(payload) => Some(payload),
            // No run artefact and no dec party to look under, or no identity
            // row for one: the keys are genuinely unrecorded. That is the
            // pre-identity-table party, and the only case a caller may treat
            // leniently. A read that *fails*, or a payload that will not
            // decode, is an error and stays one.
            None => match self.dec_party_id.as_ref() {
                Some(dec_party_id) => {
                    storage
                        .read_identity(dec_party_id, identity_kinds::PEER_PUBLIC_KEYS, &self_id)
                        .await?
                }
                None => None,
            },
        };

        let Some(payload) = stored else {
            return Ok(None);
        };

        let keys = decode_keys_payload(&payload)?;
        if keys.len() != 2 {
            anyhow::bail!(
                "expected 2 public keys for this node, found {count}",
                count = keys.len()
            );
        }
        Ok(Some(keys))
    }
}

/// The namespace fingerprint of a recorded `[namespace_key, daml_key]` bundle,
/// or `None` when index `[0]` is not a namespace key.
///
/// A bundle recovered from the chain cannot carry a namespace key — the chain
/// holds none — and older builds put a copy of the Daml key there instead.
/// Taking that at face value compares a Daml fingerprint against the party's
/// owner set, which refuses every proposal the node is asked to sign.
fn namespace_fingerprint(keys: &[SigningPublicKey]) -> Option<Fingerprint> {
    let key = keys.first()?;
    if !key.usage.contains(&(SigningKeyUsage::Namespace as i32)) {
        tracing::warn!(
            "this node's recorded key bundle for this party carries no namespace key at [0]"
        );
        return None;
    }
    Some(utils::compute_fingerprint(key))
}

/// The party's head topology state as this node reads it.
trait TopologyHead {
    async fn namespace_definition(
        &self,
        namespace: &str,
    ) -> Result<(u32, DecentralizedNamespaceDefinition)>;
    async fn p2p_mapping(&self, party: &CantonId) -> Result<(u32, PartyToParticipant)>;
    async fn party_to_key_mapping(&self, party: &CantonId) -> Result<Option<PartyToKeyMapping>>;
}

struct SynchronizerHead<'a> {
    config: &'a NodeConfig,
    synchronizer_id: String,
}

impl TopologyHead for SynchronizerHead<'_> {
    async fn namespace_definition(
        &self,
        namespace: &str,
    ) -> Result<(u32, DecentralizedNamespaceDefinition)> {
        topology::fetch_namespace_definition_at_head(self.config, &self.synchronizer_id, namespace)
            .await
    }

    async fn p2p_mapping(&self, party: &CantonId) -> Result<(u32, PartyToParticipant)> {
        topology::fetch_p2p_mapping_at_head(self.config, &self.synchronizer_id, party).await
    }

    async fn party_to_key_mapping(&self, party: &CantonId) -> Result<Option<PartyToKeyMapping>> {
        topology::fetch_party_to_key_mapping(self.config, &self.synchronizer_id, party).await
    }
}

/// The member an accepted kick invitation names. Other runs name none.
#[derive(Clone, Copy, Debug, Default)]
struct Departing<'a> {
    /// The kicked participant, when the invitation names one.
    participant: Option<&'a str>,
    /// The kicked owner key. Absent from coordinators that predate the field.
    owner_key: Option<&'a str>,
    /// What this node has on record about the owner keys, to check
    /// `owner_key` against. A kick without records refuses a named key.
    records: Option<&'a OwnerRecords>,
}

/// What this node has on record about who holds a party's owner keys.
#[derive(Clone, Debug, Default)]
struct OwnerRecords {
    /// Each host's owner key, by participant uid.
    host_keys: BTreeMap<String, String>,
    /// The participant that claimed each owner key, by owner key.
    claims: BTreeMap<String, String>,
}

/// `new_member` is this node's `(namespace, daml)` fingerprints when it is the
/// new member. `departing` is the member a kick invitation names.
///
/// The host set must match the accepted invitation, but the coordinator writes
/// the invitation, so the host changes are pinned here too. Only add-party
/// hosts a participant the on-chain mapping does not, and only one. Only a
/// kick stops hosting one, and only the participant the invitation names.
///
/// A kick removes one owner key. It removes every signing key above one per
/// host left: the host's own key, or the key a member left behind when it
/// removed its own hosting entry. A kick that removes no host must name the
/// owner key, because the unchanged hosts give a peer nothing else to tie the
/// removal to.
#[cfg(test)]
async fn check_against_head(
    head: &impl TopologyHead,
    kind: WorkflowKind,
    dec_party_id: &CantonId,
    dns: (u32, &DecentralizedNamespaceDefinition),
    p2p: (u32, &PartyToParticipant),
    new_member: Option<(&str, &str)>,
    departing: Departing<'_>,
) -> Result {
    check_keys_against_head(head, kind, dec_party_id, dns, p2p, new_member, departing)
        .await
        .map(|_| ())
}

/// See [`check_against_head`]. Returns the on-chain key set when it comes
/// from a legacy `PartyToKeyMapping`. Such a set still holds departed
/// members' keys, so the caller pins the proposal to this node's records
/// with [`check_legacy_claims`].
async fn check_keys_against_head(
    head: &impl TopologyHead,
    kind: WorkflowKind,
    dec_party_id: &CantonId,
    (dns_serial, namespace_def): (u32, &DecentralizedNamespaceDefinition),
    (p2p_serial, mapping): (u32, &PartyToParticipant),
    new_member: Option<(&str, &str)>,
    departing: Departing<'_>,
) -> Result<Option<Vec<SigningPublicKey>>> {
    let (head_dns_serial, current_def) = head
        .namespace_definition(&dec_party_id.namespace.to_hex())
        .await
        .map_err(|e| {
            anyhow::anyhow!("cannot read the current owner set of {dec_party_id}: {e:#}")
        })?;
    let (head_p2p_serial, current_p2p) = head.p2p_mapping(dec_party_id).await.map_err(|e| {
        anyhow::anyhow!("cannot read the current signing keys of {dec_party_id}: {e:#}")
    })?;

    let hosts_added = hosts_not_in(mapping, &current_p2p);
    let max_hosts_added = usize::from(kind == WorkflowKind::AddParty);
    if hosts_added.len() > max_hosts_added {
        anyhow::bail!(
            "{kind:?} proposal hosts {hosts_added:?}, which the on-chain mapping does not \
             host; it may add at most {max_hosts_added}"
        );
    }
    let departing = if kind == WorkflowKind::Kick {
        departing
    } else {
        Departing::default()
    };
    let hosts_removed = hosts_not_in(&current_p2p, mapping);
    if let Some(evicted) = hosts_removed
        .iter()
        .find(|uid| Some(**uid) != departing.participant)
    {
        anyhow::bail!(
            "{kind:?} proposal stops hosting {evicted}, which the accepted invitation does not \
             name as departing"
        );
    }
    let kicked_owner = departing.owner_key;
    if kind == WorkflowKind::Kick {
        // A DNS proposal at the head serial is the namespace already in force
        // (or refused below), so it removes nothing that needs pinning.
        if hosts_removed.is_empty() && kicked_owner.is_none() && dns_serial != head_dns_serial {
            anyhow::bail!(
                "Kick proposal removes no host, so it removes an owner that does not host the \
                 party, but the accepted invitation names no owner key to check it against"
            );
        }
        if let Some(owner) = kicked_owner {
            // The namespace in force is the one the kick leaves, so it must
            // no longer list the owner the kick removes.
            if dns_serial == head_dns_serial && current_def.owners.iter().any(|o| o == owner) {
                anyhow::bail!(
                    "Kick proposal keeps the namespace in force, which still lists the owner key \
                     {owner} that the accepted invitation removes"
                );
            }
            check_kicked_owner_key(
                owner,
                departing.participant,
                &current_p2p,
                &current_def.owners,
                departing.records.unwrap_or(&OwnerRecords::default()),
            )?;
        }
    }

    check_dns_against_head(
        kind,
        (dns_serial, namespace_def),
        (head_dns_serial, &current_def),
        new_member.map(|(namespace, _)| namespace),
        kicked_owner,
    )?;
    if already_applied(
        "P2P",
        (p2p_serial, mapping),
        (head_p2p_serial, &current_p2p),
    )? {
        return Ok(None);
    }

    let (current_keys, legacy) = signing_key_baseline(head, dec_party_id, &current_p2p).await?;

    let proposed_keys = mapping
        .party_signing_keys
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("P2P proposal carries no party signing keys"))?;

    let mut allowed = AllowedDelta::of("party signing key", kind)?;
    allowed.added = new_member.map(|(_, daml)| daml);
    if kind == WorkflowKind::Kick {
        allowed.removals = current_keys
            .len()
            .saturating_sub(mapping.participants.len());
    }
    check_set_delta(
        "party signing key",
        kind,
        allowed,
        &current_keys
            .iter()
            .map(|key| utils::compute_fingerprint(key).into())
            .collect(),
        &proposed_keys
            .keys
            .iter()
            .map(|key| utils::compute_fingerprint(key).into())
            .collect::<Vec<String>>(),
        legacy,
    )?;
    check_signing_key_attributes(kind, &current_keys, &proposed_keys.keys)?;
    Ok(legacy.then_some(current_keys))
}

/// Hold a proposal built from a legacy key set to this node's records of
/// each host's Daml key.
///
/// A legacy `PartyToKeyMapping` still holds the keys of members that left.
/// So the on-chain delta lets a proposal drop any of its keys, and cannot
/// tell a departed member's key from a current one. The coordinator keeps
/// exactly the key each host claims. The proposal must therefore carry each
/// host's recorded key and no other key. The one exception is the key of a
/// new member that this node has not recorded yet. The new member pins that
/// key itself.
fn check_legacy_claims(
    kind: WorkflowKind,
    hosts: &[String],
    new_member: Option<&str>,
    proposed: &[Fingerprint],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result {
    let mut kept: BTreeMap<&Fingerprint, &str> = BTreeMap::new();
    let mut unrecorded_new_member = false;
    for host in hosts {
        let is_new = Some(host.as_str()) == new_member;
        match claims.get(host) {
            Some(key) if proposed.contains(key) => {
                if let Some(previous) = kept.insert(key, host) {
                    anyhow::bail!(
                        "This node has {key} on record for both {previous} and {host}. Refresh \
                         /decentralized-parties on this node and retry"
                    );
                }
            }
            // The new member may come back with another key than the one
            // this node recorded for it before.
            _ if is_new => unrecorded_new_member = true,
            Some(key) => anyhow::bail!(
                "{kind:?} proposal drops {key}, which this node has on record as {host}'s Daml \
                 key"
            ),
            None => anyhow::bail!(
                "This node has no Daml key on record for {host}, so it cannot tell which keys of \
                 the legacy mapping the {kind:?} proposal may drop. Refresh \
                 /decentralized-parties on this node and retry"
            ),
        }
    }
    let unclaimed: Vec<&Fingerprint> = proposed
        .iter()
        .filter(|key| !kept.contains_key(key))
        .collect();
    let allowed = usize::from(unrecorded_new_member);
    if unclaimed.len() > allowed {
        anyhow::bail!(
            "{kind:?} proposal keeps {unclaimed:?}, which no host holds by this node's records; \
             it may keep {allowed} such key(s)"
        );
    }
    Ok(())
}

/// Each signing key a proposal keeps must equal the on-chain key, and each
/// key it adds must be a protocol key.
///
/// A fingerprint covers only the key bytes. A proposal could keep a member's
/// key bytes but drop the protocol usage, and every fingerprint check would
/// still pass. That member's key would then stop counting toward the
/// signing threshold.
fn check_signing_key_attributes(
    kind: WorkflowKind,
    current: &[SigningPublicKey],
    proposed: &[SigningPublicKey],
) -> Result {
    let on_chain: BTreeMap<Fingerprint, &SigningPublicKey> = current
        .iter()
        .map(|key| (utils::compute_fingerprint(key), key))
        .collect();
    for key in proposed {
        let fingerprint = utils::compute_fingerprint(key);
        match on_chain.get(&fingerprint) {
            Some(kept) if *kept != key => anyhow::bail!(
                "{kind:?} proposal keeps the signing key {fingerprint} but changes its usage, \
                 spec or format"
            ),
            Some(_) => {}
            None if !key.usage.contains(&(SigningKeyUsage::Protocol as i32)) => anyhow::bail!(
                "{kind:?} proposal adds the signing key {fingerprint} without protocol usage"
            ),
            None => {}
        }
    }
    Ok(())
}

/// The signing keys a proposal for `dec_party_id` is built from, and whether
/// they include a legacy `PartyToKeyMapping`.
///
/// A party onboarded before Canton 3.4 keeps its keys in a legacy
/// PartyToKeyMapping that still holds departed members' keys, so the proposal
/// adopts a subset of it rather than the whole set.
///
/// The baseline has to match what the coordinator built its proposal from, or
/// the keys it restored read as additions and the peer refuses to sign. A
/// pre-1.10.0 add left parties half-migrated, holding one member's key inline
/// and the rest in the mapping, so a short inline set means the baseline is
/// both sources together.
async fn signing_key_baseline(
    head: &impl TopologyHead,
    dec_party_id: &CantonId,
    current_p2p: &PartyToParticipant,
) -> Result<(Vec<SigningPublicKey>, bool)> {
    let members = current_p2p.participants.len();
    let inline = current_p2p
        .party_signing_keys
        .clone()
        .map(|keys| keys.keys)
        .unwrap_or_default();
    if inline.len() >= members && !inline.is_empty() {
        return Ok((inline, false));
    }
    let legacy_keys = head.party_to_key_mapping(dec_party_id).await?;
    if legacy_keys.is_none() && inline.is_empty() {
        anyhow::bail!(
            "{dec_party_id} carries neither inline signing keys nor a PartyToKeyMapping, \
             so the proposed keys cannot be checked"
        );
    }
    let mut union = legacy_keys.map(|m| m.signing_keys).unwrap_or_default();
    let mut seen: BTreeSet<Fingerprint> = union.iter().map(utils::compute_fingerprint).collect();
    for key in inline {
        if seen.insert(utils::compute_fingerprint(&key)) {
            union.push(key);
        }
    }
    Ok((union, true))
}

/// Hold a rotate-key proposal to the party's on-chain state.
///
/// The hosts stay as they are, and the key set changes by exactly one swap:
/// `rotated_key` leaves and one new key joins. Returns the on-chain key set
/// the proposal was built from, or `None` when the proposal is already in
/// force and there is nothing left to check.
async fn check_rotation_against_head(
    head: &impl TopologyHead,
    dec_party_id: &CantonId,
    (p2p_serial, mapping): (u32, &PartyToParticipant),
    rotated_key: &Fingerprint,
) -> Result<Option<Vec<SigningPublicKey>>> {
    let kind = WorkflowKind::RotateKey;
    let (head_p2p_serial, current_p2p) = head.p2p_mapping(dec_party_id).await.map_err(|e| {
        anyhow::anyhow!("cannot read the current signing keys of {dec_party_id}: {e:#}")
    })?;
    if already_applied(
        "P2P",
        (p2p_serial, mapping),
        (head_p2p_serial, &current_p2p),
    )? {
        return Ok(None);
    }

    let added = hosts_not_in(mapping, &current_p2p);
    let removed = hosts_not_in(&current_p2p, mapping);
    if !added.is_empty() || !removed.is_empty() {
        anyhow::bail!(
            "{kind:?} proposal changes the hosts (added {added:?}, removed {removed:?}); a key \
             rotation keeps them"
        );
    }
    // Same hosts is not enough: a changed permission or onboarding flag
    // would change what a host may do.
    let entries = |m: &PartyToParticipant| {
        let mut hosts = m.participants.clone();
        hosts.sort_by(|a, b| a.participant_uid.cmp(&b.participant_uid));
        hosts
    };
    if entries(mapping) != entries(&current_p2p) {
        anyhow::bail!(
            "{kind:?} proposal changes a host's permission or onboarding state; a key rotation \
             keeps them"
        );
    }
    // The invitation's thresholds come from the coordinator, so the chain
    // pins them here. A rotation keeps the signing threshold in force, even
    // where it differs from the namespace threshold.
    if mapping.threshold != current_p2p.threshold {
        anyhow::bail!(
            "{kind:?} proposal changes the confirmation threshold from {from} to {to}; a key \
             rotation keeps it",
            from = current_p2p.threshold,
            to = mapping.threshold
        );
    }
    require_complete_inline_keys(&current_p2p, dec_party_id)?;
    let legacy_mapping = if current_p2p.party_signing_keys.is_none() {
        head.party_to_key_mapping(dec_party_id).await?
    } else {
        None
    };
    let in_force = signing_threshold_in_force(&current_p2p, legacy_mapping.as_ref());
    let signing = mapping
        .party_signing_keys
        .as_ref()
        .map(|keys| keys.threshold);
    if signing != in_force {
        let shown =
            |threshold: Option<u32>| threshold.map_or("none".to_string(), |t| t.to_string());
        anyhow::bail!(
            "{kind:?} proposal changes the signing threshold from {from} to {to}; a key rotation \
             keeps it",
            from = shown(in_force),
            to = shown(signing)
        );
    }

    let (current_keys, legacy) = signing_key_baseline(head, dec_party_id, &current_p2p).await?;
    let current: BTreeSet<String> = current_keys
        .iter()
        .map(|key| utils::compute_fingerprint(key).into())
        .collect();
    if !current.contains(rotated_key.as_str()) {
        anyhow::bail!(
            "{dec_party_id} does not carry the key {rotated_key} that the accepted invitation \
             replaces"
        );
    }
    let proposed_keys = &mapping
        .party_signing_keys
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("P2P proposal carries no party signing keys"))?
        .keys;
    let proposed: Vec<String> = proposed_keys
        .iter()
        .map(|key| utils::compute_fingerprint(key).into())
        .collect();
    if proposed.iter().any(|key| key == rotated_key.as_str()) {
        anyhow::bail!("{kind:?} proposal keeps the key {rotated_key} it is meant to replace");
    }
    let additions = proposed
        .iter()
        .filter(|key| !current.contains(*key))
        .count();
    if additions != 1 {
        anyhow::bail!("{kind:?} proposal adds {additions} signing key(s); a rotation adds one");
    }

    let mut allowed = AllowedDelta::of("party signing key", kind)?;
    // A legacy baseline may also drop departed members' keys, so the removed
    // key is pinned above instead of here.
    allowed.removed = (!legacy).then_some(rotated_key.as_str());
    check_set_delta(
        "party signing key",
        kind,
        allowed,
        &current,
        &proposed,
        legacy,
    )?;
    check_signing_key_attributes(kind, &current_keys, proposed_keys)?;
    Ok(Some(current_keys))
}

/// Hold a rotation to this node's records of each member's Daml key, and
/// return the key the proposal adds for the coordinator.
///
/// Every other host must have a key on record here. That key must stay in
/// the proposal, and it must not be the key the rotation replaces. Exactly
/// one proposed key is then left, and it is the coordinator's new key. The
/// replaced key must be the coordinator's on record. A node that already
/// recorded the new key accepts it in place of the old one.
///
/// Without these checks a coordinator could swap or drop another member's
/// key, or keep a departed member's key, and so hold two of the party's keys.
fn check_rotation_claims(
    coordinator: &str,
    rotated_key: &Fingerprint,
    members: &[String],
    proposed: &[Fingerprint],
    claims: &BTreeMap<String, Fingerprint>,
) -> Result<Fingerprint> {
    let mut others: BTreeMap<&Fingerprint, &str> = BTreeMap::new();
    for member in members.iter().filter(|m| m.as_str() != coordinator) {
        let Some(claimed) = claims.get(member) else {
            anyhow::bail!(
                "This node has no Daml key on record for {member}, so it cannot tell whether \
                 the rotation keeps theirs. Refresh /decentralized-parties on this node and retry"
            );
        };
        if claimed == rotated_key {
            anyhow::bail!(
                "The rotation replaces {rotated_key}, which this node has on record as \
                 {member}'s Daml key"
            );
        }
        if !proposed.contains(claimed) {
            anyhow::bail!(
                "The rotation drops {claimed}, which this node has on record as {member}'s \
                 Daml key"
            );
        }
        if let Some(previous) = others.insert(claimed, member) {
            anyhow::bail!(
                "This node has {claimed} on record for both {previous} and {member}. Refresh \
                 /decentralized-parties on this node and retry"
            );
        }
    }

    let left: Vec<&Fingerprint> = proposed
        .iter()
        .filter(|key| !others.contains_key(key))
        .collect();
    let [new_key] = left.as_slice() else {
        anyhow::bail!(
            "The rotation leaves {count} keys that no other member holds by this node's records \
             ({left:?}); it may leave only the coordinator's new key",
            count = left.len()
        );
    };
    if *new_key == rotated_key {
        anyhow::bail!("The rotation keeps the key {rotated_key} it is meant to replace");
    }
    if let Some((holder, _)) = claims
        .iter()
        .find(|(member, key)| member.as_str() != coordinator && key == new_key)
    {
        anyhow::bail!(
            "The rotation adds {new_key}, which this node has on record as {holder}'s Daml key"
        );
    }
    match claims.get(coordinator) {
        Some(claimed) if claimed == rotated_key || claimed == *new_key => {}
        Some(claimed) => anyhow::bail!(
            "The rotation replaces {rotated_key}, but this node has {claimed} on record as \
             {coordinator}'s Daml key. If {coordinator}'s last rotation failed after it was \
             submitted, retry that run instead of starting a new one"
        ),
        None => anyhow::bail!(
            "This node has no Daml key on record for {coordinator}, so it cannot tell whether \
             {rotated_key} is theirs. Refresh /decentralized-parties on this node and retry"
        ),
    }
    Ok((*new_key).clone())
}

/// The hosts of `from` that `other` does not host.
fn hosts_not_in<'a>(from: &'a PartyToParticipant, other: &PartyToParticipant) -> Vec<&'a str> {
    let other: BTreeSet<&str> = other
        .participants
        .iter()
        .map(|p| p.participant_uid.as_str())
        .collect();
    from.participants
        .iter()
        .map(|p| p.participant_uid.as_str())
        .filter(|uid| !other.contains(uid))
        .collect()
}

/// The owner key a kick removes must belong to the member the invitation
/// names, and to no host that stays.
///
/// The coordinator picks the key from its own cache, so a peer checks it
/// against its own records. Canton counts only the namespace threshold, so a
/// host whose key goes cannot block the change alone. A host whose key this
/// node does not know could hold the named key, so the peer refuses until it
/// knows every staying host's key. A recorded key that is no longer an owner
/// is stale and counts as unknown, except for the departing host: a resumed
/// kick may have removed its key already.
///
/// A peer learns a host's key only from that host. A departing host that went
/// offline before this node recorded its key can never report it, so the peer
/// accepts the named key without a record for the departing host. Every
/// staying host's key is still known and differs from the named key, and no
/// other participant has claimed it, so no staying host loses its authority.
/// A record that names a different key still refuses.
///
/// The accepted risk: an owner that hosts nothing and that never sent this
/// node its claim is not protected. A dishonest coordinator can name that
/// owner's key in place of the departing host's key. The peer cannot narrow
/// the key down further, because a party that needs this kick often also
/// holds such an unclaimed owner, and then two keys fit.
fn check_kicked_owner_key(
    owner: &str,
    departing: Option<&str>,
    head: &PartyToParticipant,
    head_owners: &[String],
    records: &OwnerRecords,
) -> Result {
    let is_owner = |key: &str| head_owners.iter().any(|o| o == key);
    let mut departing_hosts = false;
    for host in &head.participants {
        let uid = host.participant_uid.as_str();
        let recorded = records.host_keys.get(uid).map(String::as_str);
        if Some(uid) == departing {
            departing_hosts = true;
            match recorded {
                Some(found) if found != owner => anyhow::bail!(
                    "Kick invitation removes owner key {owner} with host {uid}, but this node has \
                     {found} on record for {uid}"
                ),
                Some(_) => {}
                None => {
                    if let Some(claimant) = records.claims.get(owner)
                        && claimant != uid
                    {
                        anyhow::bail!(
                            "Kick invitation removes owner key {owner} with host {uid}, but this \
                             node has no owner key on record for {uid} and has {owner} on record \
                             as claimed by {claimant}"
                        );
                    }
                }
            }
            continue;
        }
        match recorded.filter(|key| is_owner(key)) {
            Some(key) if key == owner => anyhow::bail!(
                "Kick invitation removes owner key {owner}, which belongs to host {uid}, and {uid} \
                 stays in the party"
            ),
            Some(_) => {}
            None => anyhow::bail!(
                "Cannot tell whether owner key {owner} belongs to host {uid}: this node has no \
                 current owner key on record for {uid}. Refresh /decentralized-parties on this \
                 node and retry"
            ),
        }
    }
    if let Some(participant) = departing
        && !departing_hosts
        && let Some(claimant) = records.claims.get(owner)
        && claimant != participant
    {
        anyhow::bail!(
            "Kick invitation removes owner key {owner} with {participant}, but this node has the \
             key on record as claimed by {claimant}"
        );
    }
    Ok(())
}

/// Whether a proposal is the head transaction itself. The same serial with a
/// different mapping is refused: Canton would reject it, and the peer cannot
/// tell what the coordinator meant by it.
///
/// Any other proposal must sit at the next serial. Canton checks the serial
/// only on submission, and the coordinator holds the signatures until then,
/// so a signature on a later serial could be submitted against a state this
/// peer never checked.
fn already_applied<T: PartialEq>(
    what: &str,
    (proposal_serial, proposal): (u32, &T),
    (head_serial, head): (u32, &T),
) -> Result<bool> {
    if proposal_serial == head_serial {
        if proposal != head {
            anyhow::bail!(
                "{what} proposal reuses the on-chain serial {head_serial} but differs from the \
                 on-chain mapping"
            );
        }
        return Ok(true);
    }
    if head_serial.checked_add(1) != Some(proposal_serial) {
        anyhow::bail!(
            "{what} proposal is at serial {proposal_serial} but the on-chain serial is \
             {head_serial}; only the next serial can be signed"
        );
    }
    Ok(false)
}

fn check_dns_against_head(
    kind: WorkflowKind,
    proposal: (u32, &DecentralizedNamespaceDefinition),
    head: (u32, &DecentralizedNamespaceDefinition),
    new_member: Option<&str>,
    removed_owner: Option<&str>,
) -> Result {
    if already_applied("DNS", proposal, head)? {
        return Ok(());
    }
    let mut allowed = AllowedDelta::of("DNS owner", kind)?;
    allowed.added = new_member;
    allowed.removed = removed_owner;
    check_set_delta(
        "DNS owner",
        kind,
        allowed,
        &head.1.owners.iter().cloned().collect(),
        &proposal.1.owners,
        false,
    )
}

/// How far a run may move one on-chain set.
#[derive(Clone, Copy, Debug)]
struct AllowedDelta<'a> {
    max_added: usize,
    removals: usize,
    /// The entry the run adds, when this node knows it.
    added: Option<&'a str>,
    /// The entry the run removes, when the invitation names it.
    removed: Option<&'a str>,
}

impl AllowedDelta<'_> {
    /// The delta a run of `kind` makes by default: nothing for
    /// change-threshold, one entry removed for kick, at most one added for
    /// add-party (none when a former host is hosted again), and one swapped
    /// for rotate-key.
    fn of(what: &str, kind: WorkflowKind) -> Result<Self> {
        let (max_added, removals) = match kind {
            WorkflowKind::ChangeThreshold => (0, 0),
            WorkflowKind::Kick => (0, 1),
            WorkflowKind::AddParty => (1, 0),
            WorkflowKind::RotateKey => (1, 1),
            other => anyhow::bail!("no on-chain {what} delta is defined for a {other:?} run"),
        };
        Ok(Self {
            max_added,
            removals,
            added: None,
            removed: None,
        })
    }
}

/// The proposed set must be the current on-chain set changed only as
/// `allowed` permits.
///
/// `allowed.added` and `allowed.removed` pin the changed entry when this node
/// knows it. `legacy_source` marks a current set that may hold departed
/// members' keys, so a proposal may drop any of them but still add none beyond
/// the new member's.
fn check_set_delta(
    what: &str,
    kind: WorkflowKind,
    allowed: AllowedDelta<'_>,
    current: &BTreeSet<String>,
    proposed: &[String],
    legacy_source: bool,
) -> Result {
    let proposed_set: BTreeSet<String> = proposed.iter().cloned().collect();
    if proposed_set.len() != proposed.len() {
        anyhow::bail!("proposal repeats a {what}");
    }
    let added: Vec<&String> = proposed_set.difference(current).collect();
    let removed: Vec<&String> = current.difference(&proposed_set).collect();

    let AllowedDelta {
        max_added,
        removals,
        added: new_member,
        removed: departing,
    } = allowed;
    if added.len() > max_added {
        anyhow::bail!(
            "{kind:?} proposal adds {what}(s) {added:?} that are not on-chain; it may add at \
             most {max_added}"
        );
    }
    if !legacy_source && removed.len() != removals {
        anyhow::bail!(
            "{kind:?} proposal removes {what}(s) {removed:?} from the on-chain set; it must \
             remove exactly {removals}"
        );
    }
    if let Some(new_member) = new_member {
        if let Some(stranger) = added.iter().find(|added| added.as_str() != new_member) {
            anyhow::bail!("proposal adds {what} {stranger}, not the new member's {new_member}");
        }
        if !proposed_set.contains(new_member) {
            anyhow::bail!("proposal does not carry the new member's {what} {new_member}");
        }
    }
    if let Some(departing) = departing
        && let Some(other) = removed.iter().find(|removed| removed.as_str() != departing)
    {
        anyhow::bail!(
            "{kind:?} proposal removes {what} {other}, not the {departing} the accepted \
             invitation names"
        );
    }
    Ok(())
}

/// Refuse an onboarding proposal whose mapping already exists on the chain.
/// An onboarding peer checks the proposal against the invitation only, not
/// against an existing party, so it must not sign a change to one.
async fn require_new_mapping(config: &NodeConfig, payload: &[u8]) -> Result {
    let proposal: SignedTopologyTransaction = utils::read_first_message_from_bytes(payload)?;
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    topology::check_creates_mapping(config, &synchronizer_id, &proposal).await
}

/// Unwrap a coordinator-supplied `varint(len)||SignedTopologyTransaction`
/// blob down to the topology mapping it carries.
///
/// Only an add-or-replace is ever signed. A `Remove` carrying the right party
/// and the right members would pass every other check while tearing the
/// mapping down, so the operation is pinned here, before anything else looks
/// at the mapping.
fn decode_topology_mapping(payload: &[u8]) -> Result<topology_mapping::Mapping> {
    decode_add_replace(payload)?
        .mapping
        .and_then(|mapping| mapping.mapping)
        .ok_or_else(|| anyhow::anyhow!("topology transaction carries no mapping"))
}

fn decode_proposal_serial(payload: &[u8]) -> Result<u32> {
    Ok(decode_add_replace(payload)?.serial)
}

fn decode_add_replace(payload: &[u8]) -> Result<TopologyTransaction> {
    let signed: SignedTopologyTransaction = utils::read_first_message_from_bytes(payload)?;
    let transaction = topology_version::decode_transaction(&signed.transaction)?;

    if transaction.operation != enums::TopologyChangeOp::AddReplace as i32 {
        anyhow::bail!(
            "topology transaction is a {operation:?} operation, not an add-or-replace",
            operation = enums::TopologyChangeOp::try_from(transaction.operation)
                .map(|op| op.as_str_name().to_string())
                .unwrap_or_else(|_| format!("unknown ({})", transaction.operation))
        );
    }

    Ok(transaction)
}

fn decode_namespace_definition(payload: &[u8]) -> Result<DecentralizedNamespaceDefinition> {
    match decode_topology_mapping(payload)? {
        topology_mapping::Mapping::DecentralizedNamespaceDefinition(def) => Ok(def),
        other => anyhow::bail!(
            "expected a DecentralizedNamespaceDefinition, got {kind}",
            kind = mapping_name(&other)
        ),
    }
}

fn decode_party_to_participant(payload: &[u8]) -> Result<PartyToParticipant> {
    match decode_topology_mapping(payload)? {
        topology_mapping::Mapping::PartyToParticipant(mapping) => Ok(mapping),
        other => anyhow::bail!(
            "expected a PartyToParticipant mapping, got {kind}",
            kind = mapping_name(&other)
        ),
    }
}

/// A short name for a mapping the peer refuses to sign, so the error says what
/// arrived without dumping the whole protobuf.
fn mapping_name(mapping: &topology_mapping::Mapping) -> &'static str {
    use topology_mapping::Mapping;
    match mapping {
        Mapping::NamespaceDelegation(_) => "NamespaceDelegation",
        Mapping::DecentralizedNamespaceDefinition(_) => "DecentralizedNamespaceDefinition",
        Mapping::OwnerToKeyMapping(_) => "OwnerToKeyMapping",
        Mapping::PartyToParticipant(_) => "PartyToParticipant",
        Mapping::SynchronizerTrustCertificate(_) => "SynchronizerTrustCertificate",
        Mapping::ParticipantPermission(_) => "ParticipantPermission",
        Mapping::PartyHostingLimits(_) => "PartyHostingLimits",
        Mapping::VettedPackages(_) => "VettedPackages",
        Mapping::SynchronizerParametersState(_) => "SynchronizerParametersState",
        Mapping::MediatorSynchronizerState(_) => "MediatorSynchronizerState",
        Mapping::SequencerSynchronizerState(_) => "SequencerSynchronizerState",
        Mapping::SequencingDynamicParametersState(_) => "SequencingDynamicParametersState",
        Mapping::SynchronizerUpgradeAnnouncement(_) => "SynchronizerUpgradeAnnouncement",
        Mapping::SequencerConnectionSuccessor(_) => "SequencerConnectionSuccessor",
        _ => "an unrecognized mapping",
    }
}

/// SHA-256 of a DAR's bytes, hex encoded — the value a coordinator pins in the
/// invitation and a peer recomputes before it uploads and vets the file.
pub fn hash_dar(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeysWithThreshold,
        protocol::v30::{
            TopologyMapping, enums,
            party_to_participant::{HostingParticipant, hosting_participant},
        },
        version::v1::{UntypedVersionedMessage, untyped_versioned_message},
    };
    use prost::Message;

    use super::*;

    fn canton_id(prefix: &str, byte: u8) -> Result<CantonId> {
        let mut namespace = vec![0x12, 0x20];
        namespace.extend(std::iter::repeat_n(byte, 32));
        CantonId::parse(&format!("{prefix}::{hex}", hex = hex::encode(namespace)))
    }

    fn expectations(members: Vec<CantonId>, self_id: CantonId) -> PeerExpectations {
        PeerExpectations {
            kind: WorkflowKind::ChangeThreshold,
            contract_intents: Vec::new(),
            members: members.into_iter().collect(),
            dec_party_id: None,
            prefix: None,
            threshold: Some(2),
            confirmation_threshold: None,
            kicked_participant: None,
            kicked_owner_key: None,
            new_participant: None,
            dar_filenames: Vec::new(),
            dar_hashes: Vec::new(),
            rotated_key: None,
            coordinator_id: self_id.clone(),
            self_id,
        }
    }

    fn p2p(party: &str, hosts: &[CantonId], threshold: u32) -> PartyToParticipant {
        PartyToParticipant {
            party: party.to_string(),
            threshold,
            participants: hosts
                .iter()
                .map(|id| HostingParticipant {
                    participant_uid: id.to_string(),
                    permission: enums::ParticipantPermission::Confirmation as i32,
                    onboarding: None,
                })
                .collect(),
            party_signing_keys: None,
        }
    }

    /// One distinct Daml signing key per member — the shape every mapping
    /// this tool builds carries, and what the checks expect to see.
    fn signing_keys(count: u8, threshold: u32) -> SigningKeysWithThreshold {
        SigningKeysWithThreshold {
            keys: (1..=count)
                .map(|seed| SigningPublicKey {
                    public_key: vec![seed; 32],
                    usage: vec![SigningKeyUsage::Protocol as i32],
                    ..Default::default()
                })
                .collect(),
            threshold,
        }
    }

    /// Wrap a mapping the way the coordinator ships it, so the decode path is
    /// exercised end to end rather than around.
    fn encode_proposal(mapping: topology_mapping::Mapping) -> Vec<u8> {
        encode_proposal_with_op(mapping, enums::TopologyChangeOp::AddReplace as i32)
    }

    fn encode_proposal_with_op(mapping: topology_mapping::Mapping, operation: i32) -> Vec<u8> {
        let transaction = TopologyTransaction {
            operation,
            serial: 1,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        };
        let signed = SignedTopologyTransaction {
            transaction: UntypedVersionedMessage {
                version: 30,
                wrapper: Some(untyped_versioned_message::Wrapper::Data(
                    transaction.encode_to_vec(),
                )),
            }
            .encode_to_vec(),
            ..Default::default()
        };
        utils::encode_length_prefixed_message(&signed)
    }

    #[test]
    fn accepts_a_matching_host_set() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.check_p2p_membership(&p2p("dec::x", &[a, b], 2))
    }

    /// The attack this whole module exists for: a coordinator proposing a
    /// party hosted somewhere the operator never agreed to.
    #[test]
    fn rejects_an_unexpected_host() -> Result {
        let (a, b, mallory) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("evil", 9)?,
        );
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a, b, mallory], 2))
                .is_err()
        );
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn accepted_kick_keeps_non_peer_hosts_and_still_checks_exact_membership(
        pool: SqlitePool,
    ) -> Result {
        let (coordinator, peer, non_peer, kicked, outsider) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
            canton_id("p4", 4)?,
            canton_id("p5", 5)?,
        );
        let party = canton_id("party", 9)?;
        let config = serde_json::json!({
            "participants": [&coordinator, &peer, &non_peer],
            "participant_id": kicked,
            "namespace_fingerprint": "owner-4",
            "new_threshold": 2,
        });
        sqlx::query(
            "INSERT INTO workflow_runs \
             (instance_name, kind, role, status, current_step, step_index, step_total, \
              config_json, expected_peers_json, completed_peers_json, dec_party_id, created_at, updated_at) \
             VALUES ('accepted-quorum-kick', 'Kick', 'Peer', 'inprogress', 'Active', 0, 7, \
                     ?, '[]', '[]', ?, 0, 0)",
        )
        .bind(config.to_string())
        .bind(party.to_string())
        .execute(&pool)
        .await?;
        let mut node = NodeConfig::default();
        node.node.participant_id = Some(peer.clone());
        let accepted =
            PeerExpectations::load(&pool, "accepted-quorum-kick", &node, &coordinator).await?;
        assert_eq!(accepted.kind, WorkflowKind::Kick);
        accepted.check_p2p_membership(&p2p(
            &party.to_string(),
            &[coordinator.clone(), peer.clone(), non_peer.clone()],
            2,
        ))?;

        for hosts in [
            vec![coordinator.clone(), peer.clone()],
            vec![
                coordinator.clone(),
                peer.clone(),
                non_peer.clone(),
                outsider,
            ],
            vec![coordinator, peer, non_peer, kicked],
        ] {
            let error = accepted
                .check_p2p_membership(&p2p(&party.to_string(), &hosts, 2))
                .expect_err("an omitted or extra host must still be refused")
                .to_string();
            assert!(error.contains("host set does not match"), "{error}");
        }
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn a_peer_defends_a_known_absent_survivors_key_as_well_as_its_own(
        pool: SqlitePool,
    ) -> Result {
        let party = canton_id("party", 9)?;
        let hosts: Vec<CantonId> = (1..=4)
            .map(|tag| canton_id(&format!("p{tag}"), tag))
            .collect::<Result<_>>()?;
        let all_keys = signing_keys(4, 2).keys;
        sqlx::query(
            "INSERT INTO dec_party (party_id, prefix, threshold, updated_at) VALUES (?, ?, 2, 0)",
        )
        .bind(party.to_string())
        .bind(&party.prefix)
        .execute(&pool)
        .await?;
        for (host, key) in hosts.iter().zip(&all_keys) {
            sqlx::query("INSERT INTO dec_party_participant (dec_party_id, participant_uid, permission, signing_key) VALUES (?, ?, 'confirmation', ?)")
                .bind(party.to_string()).bind(host.to_string())
                .bind(utils::compute_fingerprint(key).to_string()).execute(&pool).await?;
        }
        let mut accepted = expectations(hosts[..3].to_vec(), hosts[1].clone());
        accepted.kind = WorkflowKind::Kick;
        accepted.dec_party_id = Some(party.clone());
        accepted.kicked_participant = Some(hosts[3].clone());
        let mut proposal = p2p(&party.to_string(), &hosts[..3], 2);
        // The signer p2 keeps its own key, while absent p3 loses authority.
        proposal.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![
                all_keys[0].clone(),
                all_keys[1].clone(),
                all_keys[3].clone(),
            ],
            threshold: 2,
        });
        let error = accepted
            .check_surviving_daml_keys(&pool, &proposal)
            .await
            .expect_err("quorum signers must defend a known absent survivor too")
            .to_string();
        assert!(error.contains(&hosts[2].to_string()), "{error}");
        assert!(
            error.contains(&utils::compute_fingerprint(&all_keys[2]).to_string()),
            "{error}"
        );
        proposal.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: all_keys[..3].to_vec(),
            threshold: 2,
        });
        accepted.check_surviving_daml_keys(&pool, &proposal).await
    }

    /// Records `keys[key]` as the signing key of `hosts[host]`, by index.
    async fn record_claims(
        pool: &SqlitePool,
        party: &CantonId,
        hosts: &[CantonId],
        keys: &[SigningPublicKey],
        claims: &[(usize, usize)],
    ) -> Result {
        for (host, key) in claims {
            sqlx::query(
                "INSERT INTO dec_party_participant \
                 (dec_party_id, participant_uid, permission, signing_key) \
                 VALUES (?, ?, 'confirmation', ?) \
                 ON CONFLICT (dec_party_id, participant_uid) \
                 DO UPDATE SET signing_key = excluded.signing_key",
            )
            .bind(party.to_string())
            .bind(hosts[*host].to_string())
            .bind(utils::compute_fingerprint(&keys[*key]).to_string())
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    /// The kicked member claims a remaining member's key. A signer ignores
    /// that claim, so the kick goes ahead, but it still refuses a proposal
    /// that drops a key a remaining member claims, whoever else claims it.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn a_peer_ignores_the_targets_claim_but_never_a_survivors(pool: SqlitePool) -> Result {
        let party = canton_id("party", 9)?;
        let hosts: Vec<CantonId> = (1..=4)
            .map(|tag| canton_id(&format!("p{tag}"), tag))
            .collect::<Result<_>>()?;
        let all_keys = signing_keys(4, 2).keys;
        sqlx::query(
            "INSERT INTO dec_party (party_id, prefix, threshold, updated_at) VALUES (?, ?, 2, 0)",
        )
        .bind(party.to_string())
        .bind(&party.prefix)
        .execute(&pool)
        .await?;
        record_claims(
            &pool,
            &party,
            &hosts,
            &all_keys,
            &[(0, 0), (1, 1), (2, 2), (3, 2)],
        )
        .await?;
        let mut accepted = expectations(hosts[..3].to_vec(), hosts[1].clone());
        accepted.kind = WorkflowKind::Kick;
        accepted.dec_party_id = Some(party.clone());
        accepted.kicked_participant = Some(hosts[3].clone());
        let mut proposal = p2p(&party.to_string(), &hosts[..3], 2);
        let proposed = |keys: [usize; 3]| SigningKeysWithThreshold {
            keys: keys.iter().map(|index| all_keys[*index].clone()).collect(),
            threshold: 2,
        };

        proposal.party_signing_keys = Some(proposed([0, 1, 2]));
        accepted.check_surviving_daml_keys(&pool, &proposal).await?;

        proposal.party_signing_keys = Some(proposed([0, 1, 3]));
        let error = accepted
            .check_surviving_daml_keys(&pool, &proposal)
            .await
            .expect_err("the target's claim cannot remove p3's key")
            .to_string();
        assert!(error.contains(&hosts[2].to_string()), "{error}");

        // Now p3 claims the key p4 claims. That key stays whatever p4 says.
        record_claims(&pool, &party, &hosts, &all_keys, &[(2, 3), (3, 3)]).await?;
        proposal.party_signing_keys = Some(proposed([0, 1, 2]));
        let error = accepted
            .check_surviving_daml_keys(&pool, &proposal)
            .await
            .expect_err("a key a remaining member claims is never removed")
            .to_string();
        assert!(error.contains(&hosts[2].to_string()), "{error}");
        assert!(
            error.contains(&utils::compute_fingerprint(&all_keys[3]).to_string()),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn rejects_dropping_a_member() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b], a.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a], 1))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_proposal_that_does_not_host_this_node() -> Result {
        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let expectations = expectations(vec![b.clone(), c.clone()], a);
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[b, c], 2))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_kicked_participant_that_is_still_hosted() -> Result {
        let (a, b, kicked) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let mut expectations = expectations(vec![a.clone(), b.clone(), kicked.clone()], a.clone());
        expectations.kicked_participant = Some(kicked.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a, b, kicked], 2))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_duplicated_host() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a.clone());
        assert!(
            expectations
                .check_p2p_membership(&p2p("dec::x", &[a.clone(), a], 1))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_threshold_the_invitation_did_not_advertise() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.check_threshold("namespace", 1, 3).is_err());
        Ok(())
    }

    /// A threshold of zero would let any single signer act for the party.
    #[test]
    fn rejects_an_out_of_range_threshold() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.threshold = Some(0);
        assert!(expectations.check_threshold("namespace", 0, 3).is_err());
        expectations.threshold = Some(4);
        assert!(expectations.check_threshold("namespace", 4, 3).is_err());
        Ok(())
    }

    #[test]
    fn accepts_the_advertised_threshold() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        expectations.check_threshold("namespace", 2, 3)
    }

    #[tokio::test]
    async fn rejects_proposals_for_another_party() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mut expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.dec_party_id = Some(canton_id("ours", 7)?);
        let their_party = canton_id("theirs", 8)?;

        let dns = encode_proposal(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            DecentralizedNamespaceDefinition {
                decentralized_namespace: their_party.namespace.to_hex(),
                threshold: 2,
                owners: vec!["one".to_string(), "two".to_string()],
            },
        ));
        let p2p_payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &their_party.to_string(),
            &[a, b],
            2,
        )));

        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        assert!(
            expectations
                .check_party_proposals(&NodeConfig::default(), &pool, "run", &dns, &p2p_payload)
                .await
                .is_err()
        );
        Ok(())
    }

    /// A payload that decodes to some other topology mapping must be refused
    /// outright rather than signed because it happened to parse.
    #[test]
    fn rejects_a_mapping_of_the_wrong_kind() -> Result {
        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            "dec::x",
            &[canton_id("p1", 1)?],
            1,
        )));
        assert!(decode_namespace_definition(&payload).is_err());
        Ok(())
    }

    /// A Remove that names the accepted party and members would pass every
    /// content check while tearing the mapping down, so the operation itself
    /// is pinned.
    #[test]
    fn rejects_a_remove_operation() -> Result {
        let payload = encode_proposal_with_op(
            topology_mapping::Mapping::PartyToParticipant(p2p("dec::x", &[canton_id("p1", 1)?], 1)),
            enums::TopologyChangeOp::Remove as i32,
        );
        assert!(decode_party_to_participant(&payload).is_err());
        Ok(())
    }

    /// The signing threshold decides how many member keys authorize a
    /// transaction for the party. Dropping it to 1 while the hosting
    /// threshold looks right would hand any single member the party.
    #[test]
    fn rejects_a_weakened_party_signing_threshold() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        // A full key set, so the failure is the threshold and not the count.
        mapping.party_signing_keys = Some(signing_keys(2, 1));
        assert!(expectations.check_p2p_thresholds(&mapping).is_err());
        Ok(())
    }

    /// A stripped `party_signing_keys` is not a missing threshold — it is a
    /// mapping that would leave the party unable to authorize anything.
    /// Submission permission lets a participant submit for the party directly.
    /// Every proposal this tool builds hosts at Confirmation, so an upgrade is
    /// a change to who can act, not a formatting detail.
    #[test]
    fn rejects_a_host_upgraded_to_submission() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        mapping.participants[1].permission = enums::ParticipantPermission::Submission as i32;
        assert!(expectations.check_p2p_membership(&mapping).is_err());
        Ok(())
    }

    /// Dropping the marker activates the new member before its ACS import
    /// finishes; moving it to an existing host suspends that host instead.
    #[test]
    fn rejects_a_misplaced_onboarding_marker() -> Result {
        let (a, b, joining) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let mut expectations = expectations(vec![a.clone(), b.clone(), joining.clone()], a.clone());
        expectations.new_participant = Some(joining.clone());

        // Marker on the wrong host.
        let mut moved = p2p("dec::x", &[a.clone(), b.clone(), joining.clone()], 2);
        moved.participants[1].onboarding = Some(hosting_participant::Onboarding::default());
        assert!(expectations.check_onboarding_markers(&moved).is_err());

        // Marker missing entirely.
        let dropped = p2p("dec::x", &[a.clone(), b.clone(), joining.clone()], 2);
        assert!(expectations.check_onboarding_markers(&dropped).is_err());

        // Marker exactly where the invitation puts it.
        let mut correct = p2p("dec::x", &[a, b, joining], 2);
        correct.participants[2].onboarding = Some(hosting_participant::Onboarding::default());
        expectations.check_onboarding_markers(&correct)
    }

    /// A run that adds nobody must not introduce an onboarding marker either.
    #[test]
    fn rejects_an_onboarding_marker_on_a_run_that_adds_nobody() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        assert!(expectations.new_participant.is_none());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        mapping.participants[0].onboarding = Some(hosting_participant::Onboarding::default());
        assert!(expectations.check_onboarding_markers(&mapping).is_err());
        Ok(())
    }

    /// Only the onboarding invite's threshold is optional on the wire. For the
    /// other kinds an absent one would let the coordinator pick any in-range
    /// value.
    #[test]
    fn rejects_an_absent_threshold_except_for_onboarding() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.threshold = None;

        expectations.kind = WorkflowKind::Kick;
        assert!(expectations.check_threshold("namespace", 2, 3).is_err());
        expectations.kind = WorkflowKind::AddParty;
        assert!(expectations.check_threshold("namespace", 2, 3).is_err());

        expectations.kind = WorkflowKind::Onboarding;
        expectations.check_threshold("namespace", 2, 3)
    }

    /// `generate_keys` derives the vault key names from the prefix, so an
    /// unchecked one lets a coordinator have this node create keys under a name
    /// the operator never accepted — or reuse the keys already sitting under
    /// another party's prefix.
    #[test]
    fn rejects_key_generation_under_an_unaccepted_prefix() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.kind = WorkflowKind::Onboarding;
        expectations.prefix = Some("treasury".to_string());

        assert!(
            expectations
                .check_onboarding_config("someone-elses-party")
                .is_err()
        );
        expectations.check_onboarding_config("treasury")?;

        // No accepted prefix at all is not a licence to pick one.
        expectations.prefix = None;
        assert!(expectations.check_onboarding_config("treasury").is_err());
        Ok(())
    }

    /// The command gate checks the workflow kind, not the step order, so a
    /// coordinator can send SignP2p before SignDns. With no namespace recorded
    /// the peer must refuse rather than fall back to the prefix check.
    #[tokio::test]
    async fn rejects_a_p2p_proposal_before_any_dns_was_signed() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a.clone());
        expectations.kind = WorkflowKind::Onboarding;
        expectations.prefix = Some("treasury".to_string());

        let party = format!("treasury::{}", canton_id("x", 9)?.namespace.to_hex());
        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &party,
            &[a],
            1,
        )));

        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        let error = expectations
            .check_onboarding_p2p(&NodeConfig::default(), &pool, "run", &payload, None)
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("has not signed a DNS proposal"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// The prefix is required on every onboarding invite, so an absent one
    /// means the invite never parsed — and the coordinator would name the party.
    #[tokio::test]
    async fn rejects_an_onboarding_p2p_when_no_prefix_was_accepted() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a.clone());
        expectations.kind = WorkflowKind::Onboarding;
        assert!(expectations.prefix.is_none());

        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &canton_id("anything", 5)?.to_string(),
            &[a],
            1,
        )));
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        assert!(
            expectations
                .check_onboarding_p2p(&NodeConfig::default(), &pool, "run", &payload, None)
                .await
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_a_proposal_with_no_party_signing_keys() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mapping = p2p("dec::x", &[a, b], 2);
        assert!(mapping.party_signing_keys.is_none());
        assert!(expectations.check_p2p_thresholds(&mapping).is_err());
        Ok(())
    }

    /// The party is required on every invitation kind that reaches this check,
    /// so an absent one means the invite never parsed — the peer cannot say
    /// what it agreed to and must not accept the coordinator's word for it.
    #[test]
    fn rejects_a_config_when_no_party_was_accepted() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.dec_party_id.is_none());
        assert!(expectations.check_dec_party(&canton_id("any", 4)?).is_err());
        Ok(())
    }

    #[test]
    fn rejects_an_add_party_config_when_no_participant_was_accepted() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.new_participant.is_none());
        assert!(
            expectations
                .check_new_participant(&canton_id("any", 4)?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn accepts_matching_hosting_and_signing_thresholds() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        mapping.party_signing_keys = Some(signing_keys(2, 2));
        expectations.check_p2p_thresholds(&mapping)
    }

    /// The bug in #428: kick dropped the departing member from
    /// `participants` but carried `party_signing_keys` over verbatim, leaving
    /// its Daml key still counting towards the party's signing threshold. A
    /// peer cannot tell whose key is whose — the mapping records no owner per
    /// key — but one key too many for the member set is exactly what that
    /// mistake looks like from here.
    #[test]
    fn rejects_a_surplus_party_signing_key() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b], 2);
        // Two remaining members, three keys — the kicked member's is still in.
        mapping.party_signing_keys = Some(signing_keys(3, 2));

        let error = expectations
            .check_p2p_thresholds(&mapping)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("3 party signing key(s) for 2 member(s)"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// The other half of #428: the operator asked for 2, the coordinator moved
    /// the hosting threshold and left the signing one at 3, and the refusal
    /// said only "proposed threshold 3 differs from the accepted 2" — which
    /// reads as though the coordinator had ignored the operator's input. The
    /// message has to say which of the two thresholds is wrong.
    #[test]
    fn names_the_signing_threshold_when_only_it_is_stale() -> Result {
        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let expectations = expectations(vec![a.clone(), b.clone(), c.clone()], a.clone());
        let mut mapping = p2p("dec::x", &[a, b, c], 2);
        mapping.party_signing_keys = Some(signing_keys(3, 3));

        let error = expectations
            .check_p2p_thresholds(&mapping)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("signing threshold 3 differs from the accepted 2"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    fn three_members_at(threshold: i32, confirmation: Option<i32>) -> Result<PeerExpectations> {
        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        let mut expectations = expectations(vec![a.clone(), b, c], a);
        expectations.threshold = Some(threshold);
        expectations.confirmation_threshold = confirmation;
        Ok(expectations)
    }

    fn three_host_mapping(confirmation: u32, signing: u32) -> Result<PartyToParticipant> {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        let mut mapping = p2p("dec::x", &hosts, confirmation);
        mapping.party_signing_keys = Some(signing_keys(3, signing));
        Ok(mapping)
    }

    /// An invitation that named two different thresholds is signed when the
    /// proposal carries exactly those two, each in its own place.
    #[test]
    fn accepts_the_two_thresholds_the_invitation_named() -> Result {
        three_members_at(1, Some(3))?.check_p2p_thresholds(&three_host_mapping(3, 1)?)
    }

    /// Each threshold is held to its own accepted value, so neither can be
    /// moved, nor the two swapped.
    #[test]
    fn refuses_thresholds_the_invitation_did_not_name() -> Result {
        let expectations = three_members_at(1, Some(3))?;
        for (confirmation, signing, expected) in [
            (2, 1, "confirmation threshold 2 differs from the accepted 3"),
            (3, 3, "signing threshold 3 differs from the accepted 1"),
            (1, 3, "confirmation threshold 1 differs from the accepted 3"),
        ] {
            let error = expectations
                .check_p2p_thresholds(&three_host_mapping(confirmation, signing)?)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
        Ok(())
    }

    /// An invitation without a confirmation threshold comes from a
    /// coordinator that always proposes one value, so a differing pair is
    /// refused. A peer built before the field applies the same rule to every
    /// invitation, so it refuses a differing pair too.
    #[test]
    fn an_invitation_without_a_confirmation_threshold_refuses_a_differing_pair() -> Result {
        let expectations = three_members_at(1, None)?;
        expectations.check_p2p_thresholds(&three_host_mapping(1, 1)?)?;

        let error = expectations
            .check_p2p_thresholds(&three_host_mapping(3, 1)?)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("confirmation threshold 3 differs from the accepted 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// Peer runs accepted before the field existed have no confirmation
    /// threshold in their stored invitation, and still load.
    #[test]
    fn a_stored_invitation_without_a_confirmation_threshold_still_decodes() -> Result {
        let before: PeerRunConfig = serde_json::from_str(
            r#"{"prefix":null,"participants":[],"dar_filenames":[],"dar_hashes":[],
               "participant_id":null,"new_participant_id":null,"new_threshold":2,
               "previous_threshold":3,"package_names":[],"contract_intents":[]}"#,
        )?;
        assert_eq!(before.new_threshold, Some(2));
        assert_eq!(before.confirmation_threshold, None);

        let after: PeerRunConfig =
            serde_json::from_str(r#"{"new_threshold":2,"confirmation_threshold":3}"#)?;
        assert_eq!(after.confirmation_threshold, Some(3));
        Ok(())
    }

    #[test]
    fn rejects_a_clearing_proposal_that_still_flags_onboarding() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let party = canton_id("dec", 7)?;
        let mut expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.dec_party_id = Some(party.clone());

        let mut mapping = p2p(&party.to_string(), &[a, b], 2);
        // Keep the mapping otherwise valid so the assertion below is about the
        // onboarding flag, not about an earlier check tripping first.
        mapping.party_signing_keys = Some(signing_keys(2, 2));
        mapping.participants[1].onboarding = Some(hosting_participant::Onboarding::default());
        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(mapping));

        let error = expectations
            .check_clear_onboarding(&payload)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("still flags"), "unexpected error: {error}");
        Ok(())
    }

    /// Canton reads the owners as a set, so a repeated namespace keeps the
    /// length the invitation expects while shrinking the real owner set — and
    /// the threshold then stands for fewer distinct signatures than agreed.
    #[tokio::test]
    async fn rejects_duplicate_dns_owners() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let party = canton_id("dec", 7)?;
        let mut expectations = expectations(vec![a.clone(), b.clone()], a.clone());
        expectations.dec_party_id = Some(party.clone());

        let dns = encode_proposal(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            DecentralizedNamespaceDefinition {
                decentralized_namespace: party.namespace.to_hex(),
                threshold: 2,
                // Two entries, one distinct owner.
                owners: vec!["same".to_string(), "same".to_string()],
            },
        ));
        let p2p_payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &party.to_string(),
            &[a, b],
            2,
        )));

        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        let error = expectations
            .check_party_proposals(&NodeConfig::default(), &pool, "run", &dns, &p2p_payload)
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("repeats an owner"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A repeated filename collapses in the set comparison and would mask a
    /// missing or extra file, so the ambiguity is refused outright.
    #[test]
    fn rejects_duplicate_dar_filenames() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string(), "gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes"), hash_dar(b"gov-bytes")];
        assert!(
            expectations
                .check_dars(&[("gov.dar".to_string(), b"gov-bytes".to_vec())])
                .is_err()
        );

        expectations.dar_filenames = vec!["a.dar".to_string(), "b.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"a"), hash_dar(b"b")];
        assert!(
            expectations
                .check_dars(&[
                    ("a.dar".to_string(), b"a".to_vec()),
                    ("a.dar".to_string(), b"a".to_vec()),
                ])
                .is_err()
        );
        Ok(())
    }

    /// Only a wholly absent hash list means "older coordinator". A partial one
    /// cannot be lined up with the filenames, so it must not read as legacy.
    #[test]
    fn rejects_a_partial_dar_hash_list() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["a.dar".to_string(), "b.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"a")];
        assert!(
            expectations
                .check_dars(&[
                    ("a.dar".to_string(), b"a".to_vec()),
                    ("b.dar".to_string(), b"b".to_vec()),
                ])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn accepts_dars_matching_the_pinned_hashes() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes")];
        expectations.check_dars(&[("gov.dar".to_string(), b"gov-bytes".to_vec())])
    }

    /// Same filename, different bytes: the DAR the operator saw named is not
    /// the DAR that arrived.
    #[test]
    fn rejects_a_swapped_dar() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes")];
        assert!(
            expectations
                .check_dars(&[("gov.dar".to_string(), b"malicious-bytes".to_vec())])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_an_extra_dar() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.dar_hashes = vec![hash_dar(b"gov-bytes")];
        assert!(
            expectations
                .check_dars(&[
                    ("gov.dar".to_string(), b"gov-bytes".to_vec()),
                    ("extra.dar".to_string(), b"whatever".to_vec()),
                ])
                .is_err()
        );
        Ok(())
    }

    /// The hole this closes: an invitation naming no DARs, accepted as
    /// harmless, followed by DAR bytes the peer would have uploaded and vetted
    /// without any check at all.
    #[test]
    fn rejects_dars_no_invitation_named() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        assert!(expectations.dar_filenames.is_empty());
        assert!(
            expectations
                .check_dars(&[("surprise.dar".to_string(), b"arbitrary-code".to_vec())])
                .is_err()
        );
        // Nothing accepted and nothing sent stays fine — that is a Dars run
        // with no files, not a bypass.
        expectations.check_dars(&[])
    }

    /// An older coordinator sends filenames but no hashes: the filename check
    /// still applies, the content check is skipped with a warning.
    #[test]
    fn tolerates_an_invitation_without_dar_hashes() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.dar_filenames = vec!["gov.dar".to_string()];
        expectations.check_dars(&[("gov.dar".to_string(), b"anything".to_vec())])?;
        assert!(
            expectations
                .check_dars(&[("other.dar".to_string(), b"anything".to_vec())])
                .is_err()
        );
        Ok(())
    }

    /// The bundle `backfill_peer_keys_from_chain` used to write: a copy of the
    /// Daml key where the namespace key belongs. Reading `[0]` as a namespace
    /// made every legacy member refuse the DNS proposal.
    #[test]
    fn placeholder_bundle_yields_no_namespace() {
        let daml = SigningPublicKey {
            public_key: vec![7; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        };

        assert!(namespace_fingerprint(&[daml.clone(), daml]).is_none());
    }

    #[test]
    fn real_bundle_yields_its_namespace_fingerprint() {
        let namespace = SigningPublicKey {
            public_key: vec![1; 32],
            usage: vec![SigningKeyUsage::Namespace as i32],
            ..Default::default()
        };
        let daml = SigningPublicKey {
            public_key: vec![2; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        };

        assert_eq!(
            namespace_fingerprint(&[namespace.clone(), daml]),
            Some(utils::compute_fingerprint(&namespace))
        );
    }

    fn owners(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn set_delta(
        what: &str,
        kind: WorkflowKind,
        current: &BTreeSet<String>,
        proposed: &[String],
        new_member: Option<&str>,
        legacy_source: bool,
    ) -> Result {
        let mut allowed = AllowedDelta::of(what, kind)?;
        allowed.added = new_member;
        check_set_delta(what, kind, allowed, current, proposed, legacy_source)
    }

    fn delta_error(
        kind: WorkflowKind,
        current: &[&str],
        proposed: &[&str],
        new_member: Option<&str>,
        legacy_source: bool,
    ) -> String {
        set_delta(
            "DNS owner",
            kind,
            &owners(current).into_iter().collect(),
            &owners(proposed),
            new_member,
            legacy_source,
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default()
    }

    #[test]
    fn change_threshold_keeps_the_on_chain_set() -> Result {
        set_delta(
            "DNS owner",
            WorkflowKind::ChangeThreshold,
            &owners(&["a", "b", "c"]).into_iter().collect(),
            &owners(&["c", "a", "b"]),
            None,
            false,
        )
    }

    /// The #420 attack: the count and the signer's own entry stay, but a
    /// non-signing member's entry is swapped for one the coordinator holds.
    #[test]
    fn change_threshold_rejects_a_swapped_entry() {
        let error = delta_error(
            WorkflowKind::ChangeThreshold,
            &["a", "b", "c"],
            &["a", "b", "x"],
            None,
            false,
        );
        assert!(
            error.contains("adds DNS owner"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn kick_removes_exactly_one_entry() -> Result {
        set_delta(
            "DNS owner",
            WorkflowKind::Kick,
            &owners(&["a", "b", "c"]).into_iter().collect(),
            &owners(&["a", "b"]),
            None,
            false,
        )
    }

    #[test]
    fn kick_rejects_a_swap_or_a_second_removal() {
        let swap = delta_error(
            WorkflowKind::Kick,
            &["a", "b", "c"],
            &["a", "x"],
            None,
            false,
        );
        assert!(swap.contains("adds DNS owner"), "unexpected error: {swap}");
        let two = delta_error(WorkflowKind::Kick, &["a", "b", "c"], &["a"], None, false);
        assert!(
            two.contains("must remove exactly 1"),
            "unexpected error: {two}"
        );
        let none = delta_error(WorkflowKind::Kick, &["a", "b"], &["a", "b"], None, false);
        assert!(
            none.contains("must remove exactly 1"),
            "unexpected error: {none}"
        );
    }

    #[test]
    fn add_party_adds_at_most_one_entry() -> Result {
        let current: BTreeSet<String> = owners(&["a", "b"]).into_iter().collect();
        set_delta(
            "DNS owner",
            WorkflowKind::AddParty,
            &current,
            &owners(&["a", "b", "n"]),
            None,
            false,
        )?;
        // A former host hosted again leaves the namespace as it is.
        set_delta(
            "DNS owner",
            WorkflowKind::AddParty,
            &current,
            &owners(&["a", "b"]),
            None,
            false,
        )
    }

    #[test]
    fn add_party_rejects_a_second_addition_or_a_removal() {
        let two = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b", "n", "x"],
            None,
            false,
        );
        assert!(two.contains("at most 1"), "unexpected error: {two}");
        let swap = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "n", "x"],
            None,
            false,
        );
        assert!(swap.contains("at most 1"), "unexpected error: {swap}");
        let dropped = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "n"],
            None,
            false,
        );
        assert!(
            dropped.contains("must remove exactly 0"),
            "unexpected error: {dropped}"
        );
    }

    #[test]
    fn add_party_pins_the_addition_to_the_new_member_when_known() -> Result {
        let error = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b", "x"],
            Some("n"),
            false,
        );
        assert!(
            error.contains("not the new member's n"),
            "unexpected error: {error}"
        );
        let missing = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b"],
            Some("n"),
            false,
        );
        assert!(
            missing.contains("does not carry the new member's"),
            "unexpected error: {missing}"
        );
        set_delta(
            "DNS owner",
            WorkflowKind::AddParty,
            &owners(&["a", "b"]).into_iter().collect(),
            &owners(&["a", "b", "n"]),
            Some("n"),
            false,
        )
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn the_new_member_refuses_without_its_own_key_bundle(pool: SqlitePool) -> Result {
        let (a, new_member) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mut expectations = expectations(vec![a], new_member.clone());
        expectations.kind = WorkflowKind::AddParty;
        expectations.new_participant = Some(new_member);

        let error = match expectations.new_member_fingerprints(&pool, "run").await {
            Ok(found) => anyhow::bail!("expected a refusal, got {found:?}"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("this node is the new member"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn an_existing_member_has_no_new_member_bundle_to_pin(pool: SqlitePool) -> Result {
        let (a, new_member) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.kind = WorkflowKind::AddParty;
        expectations.new_participant = Some(new_member);

        assert_eq!(
            expectations.new_member_fingerprints(&pool, "run").await?,
            None
        );
        Ok(())
    }

    /// A legacy PartyToKeyMapping still holds departed members' keys, so the
    /// proposal may drop several of them. Only add-party may bring in a new
    /// one, the new member's.
    #[test]
    fn a_legacy_source_allows_dropping_but_not_adding() -> Result {
        set_delta(
            "party signing key",
            WorkflowKind::ChangeThreshold,
            &owners(&["a", "b", "c", "old1", "old2"])
                .into_iter()
                .collect(),
            &owners(&["a", "b", "c"]),
            None,
            true,
        )?;
        let error = match set_delta(
            "party signing key",
            WorkflowKind::Kick,
            &owners(&["a", "b", "c", "old1"]).into_iter().collect(),
            &owners(&["a", "x"]),
            None,
            true,
        ) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("adds party signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    fn namespace(owners_list: &[&str]) -> DecentralizedNamespaceDefinition {
        DecentralizedNamespaceDefinition {
            decentralized_namespace: "dns".to_string(),
            threshold: 1,
            owners: owners(owners_list),
        }
    }

    #[test]
    fn accepts_a_kick_already_applied_at_the_proposal_serial() -> Result {
        let kicked = namespace(&["a", "b"]);
        check_dns_against_head(WorkflowKind::Kick, (2, &kicked), (2, &kicked), None, None)?;

        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mapping = p2p("party", &[a, b], 1);
        assert!(already_applied("P2P", (2, &mapping), (2, &mapping))?);
        Ok(())
    }

    #[test]
    fn refuses_a_different_mapping_at_the_head_serial() -> Result {
        let error = match check_dns_against_head(
            WorkflowKind::Kick,
            (2, &namespace(&["a", "x"])),
            (2, &namespace(&["a", "b"])),
            None,
            None,
        ) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("reuses the on-chain serial 2"),
            "unexpected error: {error}"
        );

        let (a, b, c) = (
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        );
        assert!(
            already_applied(
                "P2P",
                (2, &p2p("party", &[a.clone(), c], 1)),
                (2, &p2p("party", &[a, b], 1)),
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn refuses_a_kick_whose_head_matches_at_another_serial() -> Result {
        let kicked = namespace(&["a", "b"]);
        let error = match check_dns_against_head(
            WorkflowKind::Kick,
            (3, &kicked),
            (2, &kicked),
            None,
            None,
        ) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    fn dns_error(
        kind: WorkflowKind,
        proposal: (u32, &DecentralizedNamespaceDefinition),
        head: (u32, &DecentralizedNamespaceDefinition),
    ) -> Result<String> {
        match check_dns_against_head(kind, proposal, head, None, None) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => Ok(error.to_string()),
        }
    }

    #[test]
    fn refuses_a_change_threshold_beyond_the_next_serial() -> Result {
        let def = namespace(&["a", "b", "c"]);
        let error = dns_error(WorkflowKind::ChangeThreshold, (4, &def), (2, &def))?;
        assert!(
            error.contains("only the next serial can be signed"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn accepts_a_change_threshold_at_the_next_serial() -> Result {
        let def = namespace(&["a", "b", "c"]);
        check_dns_against_head(
            WorkflowKind::ChangeThreshold,
            (3, &def),
            (2, &def),
            None,
            None,
        )
    }

    /// The same valid kick delta at head + 1 passes, so the refusal at head + 2
    /// comes from the serial check and not from the kick's removal rule.
    #[test]
    fn the_serial_check_refuses_a_valid_kick_delta_at_a_later_serial() -> Result {
        let (head, kicked) = (namespace(&["a", "b", "c"]), namespace(&["a", "b"]));
        check_dns_against_head(WorkflowKind::Kick, (3, &kicked), (2, &head), None, None)?;
        let error = dns_error(WorkflowKind::Kick, (4, &kicked), (2, &head))?;
        assert!(
            error.contains("only the next serial can be signed"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn refuses_a_p2p_proposal_beyond_the_next_serial() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let mapping = p2p("party", &[a, b], 1);
        assert!(!already_applied("P2P", (3, &mapping), (2, &mapping))?);
        for serial in [1, 4] {
            let error = match already_applied("P2P", (serial, &mapping), (2, &mapping)) {
                Ok(applied) => anyhow::bail!("expected a refusal, got {applied}"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("P2P proposal is at serial"),
                "unexpected error: {error}"
            );
        }
        Ok(())
    }

    struct StubHead {
        dns: Option<(u32, DecentralizedNamespaceDefinition)>,
        p2p: Option<(u32, PartyToParticipant)>,
        legacy_keys: Option<Vec<SigningPublicKey>>,
    }

    impl TopologyHead for StubHead {
        async fn namespace_definition(
            &self,
            _namespace: &str,
        ) -> Result<(u32, DecentralizedNamespaceDefinition)> {
            self.dns
                .clone()
                .ok_or_else(|| anyhow::anyhow!("topology read unavailable"))
        }

        async fn p2p_mapping(&self, _party: &CantonId) -> Result<(u32, PartyToParticipant)> {
            self.p2p
                .clone()
                .ok_or_else(|| anyhow::anyhow!("topology read unavailable"))
        }

        async fn party_to_key_mapping(
            &self,
            _party: &CantonId,
        ) -> Result<Option<PartyToKeyMapping>> {
            Ok(self
                .legacy_keys
                .clone()
                .map(|signing_keys| PartyToKeyMapping {
                    signing_keys,
                    threshold: 2,
                    ..Default::default()
                }))
        }
    }

    fn keyed_p2p(keys: Option<SigningKeysWithThreshold>) -> Result<PartyToParticipant> {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        Ok(PartyToParticipant {
            party_signing_keys: keys,
            ..p2p("party", &hosts, 2)
        })
    }

    /// A change-threshold run on a three-member party whose on-chain keys are
    /// `head_keys` (inline) or, when those are absent, `legacy_keys`.
    async fn change_threshold_against(
        head_keys: Option<SigningKeysWithThreshold>,
        legacy_keys: Option<Vec<SigningPublicKey>>,
    ) -> Result {
        let def = namespace(&["a", "b", "c"]);
        let head = StubHead {
            dns: Some((2, def.clone())),
            p2p: Some((5, keyed_p2p(head_keys)?)),
            legacy_keys,
        };
        check_against_head(
            &head,
            WorkflowKind::ChangeThreshold,
            &canton_id("party", 9)?,
            (3, &def),
            (6, &keyed_p2p(Some(signing_keys(3, 2)))?),
            None,
            Departing::default(),
        )
        .await
    }

    #[tokio::test]
    async fn a_legacy_party_checks_the_keys_against_its_party_to_key_mapping() -> Result {
        let departed = signing_keys(4, 2).keys;
        change_threshold_against(None, Some(departed.clone())).await?;

        let error = match change_threshold_against(
            Some(SigningKeysWithThreshold {
                keys: departed,
                threshold: 2,
            }),
            None,
        )
        .await
        {
            Ok(()) => anyhow::bail!("an inline key set must not take the legacy rule"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 0"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_party_with_no_key_source_is_refused() -> Result {
        let error = match change_threshold_against(None, None).await {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("neither inline signing keys nor a PartyToKeyMapping"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A pre-1.10.0 add left parties holding one member's key inline and the
    /// rest in the legacy mapping. The peer baseline has to be both sources,
    /// or the keys the coordinator restores read as additions and every peer
    /// refuses to sign the repair.
    #[tokio::test]
    async fn a_half_migrated_party_checks_against_inline_and_legacy_together() -> Result {
        let all = signing_keys(3, 2).keys;
        let inline = SigningKeysWithThreshold {
            keys: all[2..].to_vec(),
            threshold: 1,
        };
        let legacy = all[..2].to_vec();

        change_threshold_against(Some(inline), Some(legacy)).await
    }

    /// A proposal that keeps a member's key bytes but drops its protocol
    /// usage would pass every fingerprint check. That key would then stop
    /// counting toward the signing threshold.
    #[tokio::test]
    async fn a_change_threshold_refuses_a_kept_key_with_another_usage() -> Result {
        let def = namespace(&["a", "b", "c"]);
        let head = StubHead {
            dns: Some((2, def.clone())),
            p2p: Some((5, keyed_p2p(Some(signing_keys(3, 2)))?)),
            legacy_keys: None,
        };
        let mut rewritten = signing_keys(3, 2);
        rewritten.keys[1].usage = vec![SigningKeyUsage::Namespace as i32];
        let error = match check_against_head(
            &head,
            WorkflowKind::ChangeThreshold,
            &canton_id("party", 9)?,
            (3, &def),
            (6, &keyed_p2p(Some(rewritten))?),
            None,
            Departing::default(),
        )
        .await
        {
            Ok(()) => anyhow::bail!("a key with another usage must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("changes its usage, spec or format"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_topology_read_refuses_the_proposal() -> Result {
        let def = namespace(&["a", "b", "c"]);
        let proposal = keyed_p2p(Some(signing_keys(3, 2)))?;
        let party = canton_id("party", 9)?;
        let heads = [
            (
                StubHead {
                    dns: None,
                    p2p: Some((5, proposal.clone())),
                    legacy_keys: None,
                },
                "cannot read the current owner set",
            ),
            (
                StubHead {
                    dns: Some((2, def.clone())),
                    p2p: None,
                    legacy_keys: None,
                },
                "cannot read the current signing keys",
            ),
        ];
        for (head, expected) in heads {
            let error = match check_against_head(
                &head,
                WorkflowKind::ChangeThreshold,
                &party,
                (3, &def),
                (6, &proposal),
                None,
                Departing::default(),
            )
            .await
            {
                Ok(()) => anyhow::bail!("expected a refusal"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains(expected), "unexpected error: {error}");
        }
        Ok(())
    }

    /// A party whose namespace has an owner that hosts nothing, as an add-party
    /// that stopped after its DNS change leaves it. Hosts p1 and p2 hold keys
    /// 1 and 2; owner "c" hosts nothing and holds no key.
    fn owner_only_head() -> Result<StubHead> {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        Ok(StubHead {
            dns: Some((2, namespace(&["a", "b", "c"]))),
            p2p: Some((
                5,
                PartyToParticipant {
                    party_signing_keys: Some(signing_keys(2, 2)),
                    ..p2p("party", &hosts, 2)
                },
            )),
            legacy_keys: None,
        })
    }

    /// The owner keys a peer of [`owner_only_head`] has on record: p1 and p2
    /// reported "a" and "b", and p3 claimed "c".
    fn owner_records() -> Result<OwnerRecords> {
        Ok(OwnerRecords {
            host_keys: [
                (canton_id("p1", 1)?.to_string(), "a".to_string()),
                (canton_id("p2", 2)?.to_string(), "b".to_string()),
            ]
            .into(),
            claims: [("c".to_string(), canton_id("p3", 3)?.to_string())].into(),
        })
    }

    async fn owner_only_kick(
        proposed_owners: &[&str],
        proposed_keys: u8,
        kicked_owner: Option<&str>,
    ) -> Result {
        owner_only_kick_as(
            proposed_owners,
            proposed_keys,
            kicked_owner,
            None,
            &owner_records()?,
        )
        .await
    }

    /// A kick from [`owner_only_head`] that keeps both hosts, with the
    /// invitation naming `participant` and a peer holding `records`.
    async fn owner_only_kick_as(
        proposed_owners: &[&str],
        proposed_keys: u8,
        kicked_owner: Option<&str>,
        participant: Option<&str>,
        records: &OwnerRecords,
    ) -> Result {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(proposed_keys, 1)),
            ..p2p("party", &hosts, 1)
        };
        check_against_head(
            &owner_only_head()?,
            WorkflowKind::Kick,
            &canton_id("party", 9)?,
            (3, &namespace(proposed_owners)),
            (6, &proposal),
            None,
            Departing {
                participant,
                owner_key: kicked_owner,
                records: Some(records),
            },
        )
        .await
    }

    fn expect_refusal(outcome: Result, expected: &str) -> Result {
        let error = match outcome {
            Ok(()) => anyhow::bail!("expected a refusal that says {expected:?}"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(expected), "unexpected error: {error}");
        Ok(())
    }

    async fn owner_only_kick_error(
        proposed_owners: &[&str],
        proposed_keys: u8,
        kicked_owner: Option<&str>,
    ) -> Result<String> {
        match owner_only_kick(proposed_owners, proposed_keys, kicked_owner).await {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(error) => Ok(error.to_string()),
        }
    }

    /// The owner-only kick removes the named owner and leaves the hosts and
    /// their keys as they are.
    #[tokio::test]
    async fn an_owner_only_kick_removes_the_named_owner_and_no_key() -> Result {
        owner_only_kick(&["a", "b"], 2, Some("c")).await
    }

    #[tokio::test]
    async fn an_owner_only_kick_refuses_to_remove_another_owner() -> Result {
        let error = owner_only_kick_error(&["a", "c"], 2, Some("c")).await?;
        assert!(
            error.contains("not the c the accepted invitation names"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// With no host leaving, nothing but the named key ties the removal to
    /// what the operator accepted.
    #[tokio::test]
    async fn an_owner_only_kick_needs_a_named_owner_key() -> Result {
        let error = owner_only_kick_error(&["a", "b"], 2, None).await?;
        assert!(
            error.contains("names no owner key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A member that removed its own hosting entry left its signing key in
    /// the party. Its kick takes that key out with its owner key.
    #[tokio::test]
    async fn an_owner_only_kick_drops_the_key_it_left_behind() -> Result {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let party = canton_id("party", 9)?;
        let head = StubHead {
            p2p: Some((
                5,
                PartyToParticipant {
                    party_signing_keys: Some(signing_keys(3, 2)),
                    ..p2p("party", &hosts, 2)
                },
            )),
            ..owner_only_head()?
        };
        let records = owner_records()?;
        let kick = async |keys: u8| {
            let proposal = PartyToParticipant {
                party_signing_keys: Some(signing_keys(keys, 1)),
                ..p2p("party", &hosts, 1)
            };
            check_against_head(
                &head,
                WorkflowKind::Kick,
                &party,
                (3, &namespace(&["a", "b"])),
                (6, &proposal),
                None,
                Departing {
                    owner_key: Some("c"),
                    records: Some(&records),
                    ..Departing::default()
                },
            )
            .await
        };

        kick(2).await?;
        let error = match kick(3).await {
            Ok(()) => anyhow::bail!("the leftover key must go"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// An owner that hosts nothing and left no key behind must not take a
    /// host's key with it.
    #[tokio::test]
    async fn an_owner_only_kick_refuses_to_drop_a_signing_key() -> Result {
        let error = owner_only_kick_error(&["a", "b"], 1, Some("c")).await?;
        assert!(
            error.contains("must remove exactly 0"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A coordinator built before #448 sends a kick already in force at the
    /// head serials, and names no owner key. Nothing is left to pin, so the
    /// peer signs it as before.
    #[tokio::test]
    async fn a_kick_already_in_force_needs_no_named_owner() -> Result {
        let head = owner_only_head()?;
        let (dns, p2p) = (
            head.dns
                .clone()
                .ok_or_else(|| anyhow::anyhow!("stub DNS"))?,
            head.p2p
                .clone()
                .ok_or_else(|| anyhow::anyhow!("stub P2P"))?,
        );
        check_against_head(
            &head,
            WorkflowKind::Kick,
            &canton_id("party", 9)?,
            (dns.0, &dns.1),
            (p2p.0, &p2p.1),
            None,
            Departing::default(),
        )
        .await
    }

    /// A host kick on a party that also has an owner hosting nothing still
    /// removes exactly the host's owner key and signing key.
    #[tokio::test]
    async fn a_host_kick_beside_an_owner_only_member_drops_one_key() -> Result {
        let (p1, p2) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let proposal = |keys: u8| PartyToParticipant {
            party_signing_keys: Some(signing_keys(keys, 1)),
            ..p2p("party", std::slice::from_ref(&p1), 1)
        };
        let party = canton_id("party", 9)?;
        let kick = |keys: u8| {
            let head = owner_only_head();
            let records = owner_records();
            let proposal = proposal(keys);
            let party = party.clone();
            let kicked = p2.to_string();
            async move {
                let records = records?;
                check_against_head(
                    &head?,
                    WorkflowKind::Kick,
                    &party,
                    (3, &namespace(&["a", "c"])),
                    (6, &proposal),
                    None,
                    Departing {
                        participant: Some(&kicked),
                        owner_key: Some("b"),
                        records: Some(&records),
                    },
                )
                .await
            }
        };
        kick(1).await?;
        let error = match kick(2).await {
            Ok(()) => anyhow::bail!("expected a refusal, {p2} kept its key"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A kick of host p2 from [`owner_only_head`], whose namespace in force is
    /// `head_owners` at serial 2. The invitation names `owner` as p2's key.
    async fn host_kick(
        head_owners: &[&str],
        (dns_serial, owners): (u32, &[&str]),
        owner: &str,
        records: &OwnerRecords,
    ) -> Result {
        let p1 = canton_id("p1", 1)?;
        let kicked = canton_id("p2", 2)?.to_string();
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(1, 1)),
            ..p2p("party", std::slice::from_ref(&p1), 1)
        };
        let head = StubHead {
            dns: Some((2, namespace(head_owners))),
            ..owner_only_head()?
        };
        check_against_head(
            &head,
            WorkflowKind::Kick,
            &canton_id("party", 9)?,
            (dns_serial, &namespace(owners)),
            (6, &proposal),
            None,
            Departing {
                participant: Some(&kicked),
                owner_key: Some(owner),
                records: Some(records),
            },
        )
        .await
    }

    /// A departing host that never reported its key to this node, for example
    /// because it went offline first, can still be kicked. Every staying
    /// host's key is known and differs from the named one.
    #[tokio::test]
    async fn a_host_kick_accepts_an_unrecorded_departing_key() -> Result {
        let mut records = owner_records()?;
        records.host_keys.remove(&canton_id("p2", 2)?.to_string());
        host_kick(&["a", "b", "c"], (3, &["a", "c"]), "b", &records).await
    }

    /// The accepted risk, pinned on purpose: without a record for the
    /// departing host, the peer accepts the key of an owner that hosts nothing
    /// and that nobody claimed. A change to this rule must change this test.
    #[tokio::test]
    async fn an_unrecorded_departing_key_accepts_an_unclaimed_owners_key() -> Result {
        let mut records = owner_records()?;
        records.host_keys.remove(&canton_id("p2", 2)?.to_string());
        records.claims.clear();
        host_kick(&["a", "b", "c"], (3, &["a", "b"]), "c", &records).await
    }

    /// Without a record for the departing host, the named key may not be one
    /// that another participant has claimed.
    #[tokio::test]
    async fn an_unrecorded_departing_key_may_not_be_claimed_by_another() -> Result {
        let mut records = owner_records()?;
        records.host_keys.remove(&canton_id("p2", 2)?.to_string());
        expect_refusal(
            host_kick(&["a", "b", "c"], (3, &["a", "b"]), "c", &records).await,
            "as claimed by p3::",
        )
    }

    /// Without a record for the departing host, the named key still may not
    /// be a staying host's key.
    #[tokio::test]
    async fn an_unrecorded_departing_key_may_not_be_a_staying_hosts() -> Result {
        let mut records = owner_records()?;
        records.host_keys.remove(&canton_id("p2", 2)?.to_string());
        expect_refusal(
            host_kick(&["a", "b", "c"], (3, &["b", "c"]), "a", &records).await,
            "which belongs to host p1::",
        )
    }

    /// A DNS proposal equal to the namespace in force would leave p2's key
    /// "b" in it while p2 stops hosting the party.
    #[tokio::test]
    async fn a_host_kick_refuses_to_keep_the_kicked_owner_key() -> Result {
        let head = ["a", "b", "c"];
        expect_refusal(
            host_kick(&head, (2, &head), "b", &owner_records()?).await,
            "still lists the owner key b",
        )
    }

    /// A kick resumed after its DNS change sends the namespace in force,
    /// which no longer lists p2's key.
    #[tokio::test]
    async fn a_resumed_host_kick_accepts_the_namespace_in_force() -> Result {
        let head = ["a", "c"];
        host_kick(&head, (2, &head), "b", &owner_records()?).await
    }

    /// Canton counts only the namespace threshold, so p2's own refusal does
    /// not stop a kick that names p2's key "b" as an owner-only member's.
    #[tokio::test]
    async fn an_owner_only_kick_refuses_the_key_of_a_staying_host() -> Result {
        expect_refusal(
            owner_only_kick(&["a", "c"], 2, Some("b")).await,
            "which belongs to host p2::",
        )
    }

    /// A host whose current key this node does not know may hold the named
    /// key. A recorded key that is no longer an owner is stale.
    #[tokio::test]
    async fn an_owner_only_kick_needs_every_hosts_current_key() -> Result {
        let p2 = canton_id("p2", 2)?.to_string();
        for p2_key in [None, Some("rotated")] {
            let mut records = owner_records()?;
            records.host_keys.remove(&p2);
            if let Some(key) = p2_key {
                records.host_keys.insert(p2.clone(), key.to_string());
            }
            expect_refusal(
                owner_only_kick_as(&["a", "b"], 2, Some("c"), None, &records).await,
                "no current owner key on record for p2::",
            )?;
        }
        Ok(())
    }

    /// When this node has the departing host's key on record, the owner key
    /// the invitation names must be that key.
    #[tokio::test]
    async fn a_host_kick_needs_the_hosts_recorded_key() -> Result {
        expect_refusal(
            host_kick(&["a", "b", "c"], (3, &["a", "b"]), "c", &owner_records()?).await,
            "this node has b on record for p2::",
        )
    }

    #[tokio::test]
    async fn an_owner_only_kick_matches_the_recorded_claimant() -> Result {
        let (p3, p4) = (
            canton_id("p3", 3)?.to_string(),
            canton_id("p4", 4)?.to_string(),
        );
        let records = owner_records()?;
        owner_only_kick_as(&["a", "b"], 2, Some("c"), Some(&p3), &records).await?;
        expect_refusal(
            owner_only_kick_as(&["a", "b"], 2, Some("c"), Some(&p4), &records).await,
            "on record as claimed by p3::",
        )
    }

    /// A change of threshold on a party with an owner that hosts nothing keeps
    /// every owner and every key.
    #[tokio::test]
    async fn a_change_threshold_beside_an_owner_only_member_is_accepted() -> Result {
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(2, 1)),
            ..p2p("party", &hosts, 1)
        };
        check_against_head(
            &owner_only_head()?,
            WorkflowKind::ChangeThreshold,
            &canton_id("party", 9)?,
            (3, &namespace(&["a", "b", "c"])),
            (6, &proposal),
            None,
            Departing::default(),
        )
        .await
    }

    /// A proposal from `kind` that hosts `hosts` with one key each. The party
    /// is [`owner_only_head`] with owner "c" still holding key 3, so a new
    /// host can take that key over and no key check catches it.
    async fn host_set_change(kind: WorkflowKind, hosts: &[CantonId], owners: &[&str]) -> Result {
        let head = StubHead {
            p2p: Some((
                5,
                PartyToParticipant {
                    party_signing_keys: Some(signing_keys(3, 2)),
                    ..p2p("party", &[canton_id("p1", 1)?, canton_id("p2", 2)?], 2)
                },
            )),
            ..owner_only_head()?
        };
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(u8::try_from(hosts.len())?, 1)),
            ..p2p("party", hosts, 1)
        };
        check_against_head(
            &head,
            kind,
            &canton_id("party", 9)?,
            (3, &namespace(owners)),
            (6, &proposal),
            None,
            Departing::default(),
        )
        .await
    }

    fn expect_host_refusal(outcome: Result, what: &str) -> Result {
        let error = match outcome {
            Ok(()) => anyhow::bail!("{what} must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("which the on-chain mapping does not host"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A kick that drops p2 and hosts p3 instead would hand p3 the party,
    /// even when the invitation lists p3.
    #[tokio::test]
    async fn a_kick_refuses_to_host_a_new_participant() -> Result {
        let swapped = [canton_id("p1", 1)?, canton_id("p3", 3)?];
        expect_host_refusal(
            host_set_change(WorkflowKind::Kick, &swapped, &["a", "c"]).await,
            "a kick that hosts p3",
        )
    }

    #[tokio::test]
    async fn a_change_threshold_refuses_to_host_a_new_participant() -> Result {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        expect_host_refusal(
            host_set_change(WorkflowKind::ChangeThreshold, &hosts, &["a", "b", "c"]).await,
            "a change of threshold that hosts p3",
        )
    }

    fn expect_eviction_refusal(outcome: Result, what: &str) -> Result {
        let error = match outcome {
            Ok(()) => anyhow::bail!("{what} must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("stops hosting p2::"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A kick of owner "c", which hosts nothing, that also stops hosting p2.
    /// The invitation lists only p1, so p2's absence matches it, but the
    /// invitation names c's member as departing, not p2.
    #[tokio::test]
    async fn a_kick_stops_hosting_only_the_participant_it_names() -> Result {
        let p1 = canton_id("p1", 1)?;
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(1, 1)),
            ..p2p("party", std::slice::from_ref(&p1), 1)
        };
        let claimant = canton_id("p3", 3)?.to_string();
        let records = owner_records()?;
        for participant in [None, Some(claimant.as_str())] {
            expect_eviction_refusal(
                check_against_head(
                    &owner_only_head()?,
                    WorkflowKind::Kick,
                    &canton_id("party", 9)?,
                    (3, &namespace(&["a", "b"])),
                    (6, &proposal),
                    None,
                    Departing {
                        participant,
                        owner_key: Some("c"),
                        records: Some(&records),
                    },
                )
                .await,
                "a kick of c that drops p2",
            )?;
        }
        Ok(())
    }

    /// A legacy party's keys allow any removal, so only the host check stops
    /// a change of threshold from dropping p3.
    #[tokio::test]
    async fn a_change_threshold_refuses_to_stop_hosting_a_participant() -> Result {
        let def = namespace(&["a", "b", "c"]);
        let head = StubHead {
            dns: Some((2, def.clone())),
            p2p: Some((5, keyed_p2p(None)?)),
            legacy_keys: Some(signing_keys(3, 2).keys),
        };
        let hosts = [canton_id("p1", 1)?, canton_id("p2", 2)?];
        let proposal = PartyToParticipant {
            party_signing_keys: Some(signing_keys(2, 2)),
            ..p2p("party", &hosts, 2)
        };
        let error = match check_against_head(
            &head,
            WorkflowKind::ChangeThreshold,
            &canton_id("party", 9)?,
            (3, &def),
            (6, &proposal),
            None,
            Departing::default(),
        )
        .await
        {
            Ok(()) => anyhow::bail!("a change of threshold that drops p3 must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("stops hosting p3::"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_add_party_hosts_at_most_one_new_participant() -> Result {
        let one = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        let two = [one.as_slice(), &[canton_id("p4", 4)?]].concat();
        let owners = ["a", "b", "c", "d"];
        host_set_change(WorkflowKind::AddParty, &one, &owners).await?;
        expect_host_refusal(
            host_set_change(WorkflowKind::AddParty, &two, &owners).await,
            "an add-party that hosts p3 and p4",
        )
    }

    /// The owner set may outnumber the members: a member can own the
    /// namespace without hosting the party.
    #[test]
    fn a_party_dns_may_list_more_owners_than_members() -> Result {
        let (a, b) = (canton_id("p1", 1)?, canton_id("p2", 2)?);
        let party = canton_id("dec", 7)?;
        let expectations = expectations(vec![a.clone(), b], a);
        expectations.check_party_dns_shape(
            &party,
            &DecentralizedNamespaceDefinition {
                decentralized_namespace: party.namespace.to_hex(),
                threshold: 2,
                owners: owners(&["a", "b", "c"]),
            },
        )
    }

    /// Without a named owner key, nothing ties the owners a kick keeps to the
    /// members it keeps, so the peer counts them as before keys were named.
    #[test]
    fn a_kick_without_an_owner_key_keeps_one_owner_per_member() -> Result {
        let p1 = canton_id("p1", 1)?;
        let party = canton_id("dec", 7)?;
        let dns = |list: &[&str]| DecentralizedNamespaceDefinition {
            decentralized_namespace: party.namespace.to_hex(),
            threshold: 1,
            owners: owners(list),
        };
        let mut kick = PeerExpectations {
            kind: WorkflowKind::Kick,
            threshold: Some(1),
            ..expectations(vec![p1.clone()], p1)
        };
        kick.check_party_dns_shape(&party, &dns(&["a"]))?;
        let error = match kick.check_party_dns_shape(&party, &dns(&["a", "b"])) {
            Ok(()) => anyhow::bail!("two owners for one member must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("names 1 members and no owner key"),
            "unexpected error: {error}"
        );
        kick.kicked_owner_key = Some("c".to_string());
        kick.check_party_dns_shape(&party, &dns(&["a", "b"]))
    }

    #[test]
    fn rejects_a_repeated_entry() {
        let error = delta_error(
            WorkflowKind::AddParty,
            &["a", "b"],
            &["a", "b", "b"],
            None,
            false,
        );
        assert!(
            error.contains("repeats a DNS owner"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_a_delta_for_a_kind_without_one() {
        let error = delta_error(WorkflowKind::Onboarding, &[], &["a"], None, false);
        assert!(error.contains("no on-chain"), "unexpected error: {error}");
    }

    fn key_of(seed: u8) -> SigningPublicKey {
        SigningPublicKey {
            public_key: vec![seed; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        }
    }

    fn keyed(seeds: &[u8]) -> SigningKeysWithThreshold {
        SigningKeysWithThreshold {
            keys: seeds.iter().copied().map(key_of).collect(),
            threshold: 2,
        }
    }

    /// A rotation on a three-member party whose on-chain keys are `head`
    /// (inline, at serial 5). The proposal carries `proposed` at `serial`
    /// and replaces the key of seed `rotated`.
    async fn rotation_against(
        head: Option<&[u8]>,
        legacy: Option<&[u8]>,
        (serial, proposed): (u32, &PartyToParticipant),
        rotated: u8,
    ) -> Result<Option<Vec<SigningPublicKey>>> {
        let stub = StubHead {
            dns: Some((
                2,
                DecentralizedNamespaceDefinition {
                    threshold: 2,
                    ..namespace(&["a", "b", "c"])
                },
            )),
            p2p: Some((5, keyed_p2p(head.map(keyed))?)),
            legacy_keys: legacy.map(|seeds| seeds.iter().copied().map(key_of).collect()),
        };
        check_rotation_against_head(
            &stub,
            &canton_id("party", 9)?,
            (serial, proposed),
            &utils::compute_fingerprint(&key_of(rotated)),
        )
        .await
    }

    async fn rotation_error(proposed: &PartyToParticipant, rotated: u8) -> Result<String> {
        match rotation_against(Some(&[1, 2, 3]), None, (6, proposed), rotated).await {
            Ok(_) => anyhow::bail!("expected the rotation to be refused"),
            Err(error) => Ok(error.to_string()),
        }
    }

    #[tokio::test]
    async fn a_rotation_may_swap_the_named_key_for_one_new_key() -> Result {
        let proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        let baseline = rotation_against(Some(&[1, 2, 3]), None, (6, &proposed), 2).await?;
        assert_eq!(baseline.map(|keys| keys.len()), Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_must_drop_the_key_the_invitation_names() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[9, 2, 3])))?, 2).await?;
        assert!(error.contains("keeps the key"), "unexpected error: {error}");
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_adds_exactly_one_key() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[8, 9, 3])))?, 2).await?;
        assert!(
            error.contains("adds 2 signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// Dropping another member's key with the named one would leave that
    /// member unable to sign for the party.
    #[tokio::test]
    async fn a_rotation_removes_no_key_but_the_named_one() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[9, 3])))?, 2).await?;
        assert!(
            error.contains("it must remove exactly 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_that_adds_no_key_is_refused() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[1, 3])))?, 2).await?;
        assert!(
            error.contains("adds 0 signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A coordinator could keep each member's key bytes and drop their
    /// protocol usage. Its own new key would then be the only key that
    /// counts toward the signing threshold.
    #[tokio::test]
    async fn a_rotation_keeps_each_other_key_as_it_is_on_chain() -> Result {
        let mut rewritten = keyed(&[1, 9, 3]);
        rewritten.keys[0].usage = vec![SigningKeyUsage::Namespace as i32];
        let error = rotation_error(&keyed_p2p(Some(rewritten))?, 2).await?;
        assert!(
            error.contains("changes its usage, spec or format"),
            "unexpected error: {error}"
        );

        let mut new_key = keyed(&[1, 9, 3]);
        new_key.keys[1].usage = vec![SigningKeyUsage::Namespace as i32];
        let error = rotation_error(&keyed_p2p(Some(new_key))?, 2).await?;
        assert!(
            error.contains("adds the signing key") && error.contains("without protocol usage"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A half-migrated party counts only its inline keys. A rotation would
    /// put every member's key inline at the inline threshold, so more
    /// members could sign alone.
    #[tokio::test]
    async fn a_rotation_of_a_half_migrated_party_is_refused() -> Result {
        let stub = StubHead {
            dns: Some((
                2,
                DecentralizedNamespaceDefinition {
                    threshold: 2,
                    ..namespace(&["a", "b", "c"])
                },
            )),
            p2p: Some((
                5,
                keyed_p2p(Some(SigningKeysWithThreshold {
                    threshold: 1,
                    ..keyed(&[2])
                }))?,
            )),
            legacy_keys: Some(vec![key_of(1), key_of(3)]),
        };
        let proposed = keyed_p2p(Some(SigningKeysWithThreshold {
            threshold: 1,
            ..keyed(&[1, 9, 3])
        }))?;
        let error = match check_rotation_against_head(
            &stub,
            &canton_id("party", 9)?,
            (6, &proposed),
            &utils::compute_fingerprint(&key_of(2)),
        )
        .await
        {
            Ok(_) => anyhow::bail!("a half-migrated party must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("1 inline signing key(s) for 3 hosts")
                && error.contains("Run change-threshold first"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// The invitation pins both thresholds, and a rotation changes neither.
    #[test]
    fn a_rotation_keeps_both_thresholds() -> Result {
        let hosts = [
            canton_id("p1", 1)?,
            canton_id("p2", 2)?,
            canton_id("p3", 3)?,
        ];
        let rotation = PeerExpectations {
            kind: WorkflowKind::RotateKey,
            confirmation_threshold: Some(2),
            ..expectations(hosts.to_vec(), hosts[0].clone())
        };
        let swapped = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        rotation.check_p2p_thresholds(&swapped)?;

        let confirmation = PartyToParticipant {
            threshold: 3,
            ..swapped.clone()
        };
        let signing = PartyToParticipant {
            party_signing_keys: Some(SigningKeysWithThreshold {
                threshold: 1,
                ..keyed(&[1, 9, 3])
            }),
            ..swapped
        };
        for (proposal, expected) in [
            (
                confirmation,
                "confirmation threshold 3 differs from the accepted 2",
            ),
            (signing, "signing threshold 1 differs from the accepted 2"),
        ] {
            let error = match rotation.check_p2p_thresholds(&proposal) {
                Ok(()) => anyhow::bail!("expected a refusal: {expected}"),
                Err(e) => e.to_string(),
            };
            assert!(error.contains(expected), "unexpected error: {error}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_keeps_the_hosts() -> Result {
        let mut proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        proposed.participants[2].participant_uid = canton_id("p4", 4)?.to_string();
        let error = rotation_error(&proposed, 2).await?;
        assert!(
            error.contains("changes the hosts"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_of_a_key_the_party_does_not_carry_is_refused() -> Result {
        let error = rotation_error(&keyed_p2p(Some(keyed(&[1, 9, 3])))?, 7).await?;
        assert!(
            error.contains("does not carry the key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_rotation_only_signs_the_next_serial() -> Result {
        let proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        let error = match rotation_against(Some(&[1, 2, 3]), None, (7, &proposed), 2).await {
            Ok(_) => anyhow::bail!("a serial gap must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("only the next serial"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A retried peer may be handed the proposal after it went into force.
    #[tokio::test]
    async fn a_rotation_already_in_force_has_nothing_left_to_check() -> Result {
        let in_force = keyed_p2p(Some(keyed(&[1, 2, 3])))?;
        assert!(
            rotation_against(Some(&[1, 2, 3]), None, (5, &in_force), 2)
                .await?
                .is_none()
        );
        Ok(())
    }

    /// A legacy party moves its keys inline in the same proposal, which also
    /// drops a departed member's key. Only the new key may be added.
    #[tokio::test]
    async fn a_legacy_rotation_may_drop_departed_keys_but_adds_only_the_new_one() -> Result {
        let proposed = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        rotation_against(None, Some(&[1, 2, 3, 4]), (6, &proposed), 2).await?;

        let stranger = keyed_p2p(Some(keyed(&[8, 9, 3])))?;
        let error = match rotation_against(None, Some(&[1, 2, 3, 4]), (6, &stranger), 2).await {
            Ok(_) => anyhow::bail!("two new keys must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("adds 2 signing key"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    fn member_claims(entries: &[(&str, u8)]) -> BTreeMap<String, Fingerprint> {
        entries
            .iter()
            .map(|(member, seed)| {
                (
                    (*member).to_string(),
                    utils::compute_fingerprint(&key_of(*seed)),
                )
            })
            .collect()
    }

    fn legacy_claims_error(
        kind: WorkflowKind,
        hosts: &[&str],
        new_member: Option<&str>,
        proposed: &[u8],
        claims: &[(&str, u8)],
    ) -> Result<String> {
        let hosts: Vec<String> = hosts.iter().map(ToString::to_string).collect();
        match check_legacy_claims(
            kind,
            &hosts,
            new_member,
            &fingerprints(proposed),
            &member_claims(claims),
        ) {
            Ok(()) => anyhow::bail!("expected a refusal"),
            Err(e) => Ok(e.to_string()),
        }
    }

    /// A legacy mapping still holds departed members' keys, so the on-chain
    /// delta lets a proposal drop any of them. A coordinator could drop a
    /// current member's key and keep a departed one. The records pin it.
    #[test]
    fn a_legacy_proposal_keeps_exactly_the_hosts_recorded_keys() -> Result {
        let claims = member_claims(&[("a", 1), ("b", 2), ("gone", 3)]);
        let hosts = ["a".to_string(), "b".to_string()];
        check_legacy_claims(
            WorkflowKind::ChangeThreshold,
            &hosts,
            None,
            &fingerprints(&[1, 2]),
            &claims,
        )?;

        let error = legacy_claims_error(
            WorkflowKind::Kick,
            &["a", "b"],
            None,
            &[1, 3],
            &[("a", 1), ("b", 2), ("gone", 3)],
        )?;
        assert!(
            error.contains("drops") && error.contains("on record as b's Daml key"),
            "{error}"
        );

        let error = legacy_claims_error(
            WorkflowKind::Kick,
            &["a", "b"],
            None,
            &[1, 2, 3],
            &[("a", 1), ("b", 2), ("gone", 3)],
        )?;
        assert!(error.contains("which no host holds"), "{error}");
        Ok(())
    }

    /// The new member's key may be unrecorded, or differ from a key this node
    /// recorded for it before a kick. Only that one key may be unclaimed.
    #[test]
    fn a_legacy_add_party_allows_only_the_new_members_key_unclaimed() -> Result {
        let hosts = ["a".to_string(), "b".to_string(), "new".to_string()];
        for claims in [
            member_claims(&[("a", 1), ("b", 2)]),
            member_claims(&[("a", 1), ("b", 2), ("new", 7)]),
        ] {
            check_legacy_claims(
                WorkflowKind::AddParty,
                &hosts,
                Some("new"),
                &fingerprints(&[1, 2, 9]),
                &claims,
            )?;
        }

        let error = legacy_claims_error(
            WorkflowKind::AddParty,
            &["a", "b", "new"],
            Some("new"),
            &[1, 2, 9, 3],
            &[("a", 1), ("b", 2), ("gone", 3)],
        )?;
        assert!(error.contains("it may keep 1 such key"), "{error}");
        Ok(())
    }

    #[test]
    fn a_legacy_proposal_needs_a_record_of_each_host() -> Result {
        let error = legacy_claims_error(
            WorkflowKind::ChangeThreshold,
            &["a", "b"],
            None,
            &[1, 2],
            &[("a", 1)],
        )?;
        assert!(
            error.contains("no Daml key on record for b") && error.contains("Refresh"),
            "{error}"
        );

        let error = legacy_claims_error(
            WorkflowKind::ChangeThreshold,
            &["a", "b"],
            None,
            &[1, 2],
            &[("a", 1), ("b", 1)],
        )?;
        assert!(error.contains("on record for both a and b"), "{error}");
        Ok(())
    }

    /// A coordinator could otherwise set either threshold through its own
    /// invitation, because the invitation only repeats what it chose.
    #[tokio::test]
    async fn a_rotation_keeps_both_thresholds_on_chain() -> Result {
        let confirmation = PartyToParticipant {
            threshold: 1,
            ..keyed_p2p(Some(keyed(&[1, 9, 3])))?
        };
        let error = rotation_error(&confirmation, 2).await?;
        assert!(
            error.contains("changes the confirmation threshold from 2 to 1"),
            "unexpected error: {error}"
        );

        let signing = keyed_p2p(Some(SigningKeysWithThreshold {
            threshold: 1,
            ..keyed(&[1, 9, 3])
        }))?;
        let error = rotation_error(&signing, 2).await?;
        assert!(
            error.contains("changes the signing threshold from 2 to 1"),
            "unexpected error: {error}"
        );

        // A legacy party keeps its mapping's threshold too.
        let error = match rotation_against(None, Some(&[1, 2, 3, 4]), (6, &signing), 2).await {
            Ok(_) => anyhow::bail!("a legacy rotation must keep the legacy threshold"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("changes the signing threshold from 2 to 1"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A permission or onboarding flag is part of what a host may do, so a
    /// rotation that keeps the hosts must keep those too.
    #[tokio::test]
    async fn a_rotation_keeps_each_hosts_permission_and_onboarding_state() -> Result {
        let mut observer = keyed_p2p(Some(keyed(&[1, 9, 3])))?;
        observer.participants[0].permission = enums::ParticipantPermission::Observation as i32;
        let error = rotation_error(&observer, 2).await?;
        assert!(
            error.contains("changes a host's permission or onboarding state"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// A party whose signing threshold differs from its namespace threshold
    /// keeps its signing threshold. A rotation must not move it to the
    /// namespace value.
    #[tokio::test]
    async fn a_rotation_keeps_the_signing_threshold_in_force() -> Result {
        let stub = StubHead {
            dns: Some((
                2,
                DecentralizedNamespaceDefinition {
                    threshold: 2,
                    ..namespace(&["a", "b", "c"])
                },
            )),
            p2p: Some((
                5,
                keyed_p2p(Some(SigningKeysWithThreshold {
                    threshold: 3,
                    ..keyed(&[1, 2, 3])
                }))?,
            )),
            legacy_keys: None,
        };
        let error = match check_rotation_against_head(
            &stub,
            &canton_id("party", 9)?,
            (6, &keyed_p2p(Some(keyed(&[1, 9, 3])))?),
            &utils::compute_fingerprint(&key_of(2)),
        )
        .await
        {
            Ok(_) => anyhow::bail!("lowering the signing threshold must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("changes the signing threshold from 3"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    fn fingerprints(seeds: &[u8]) -> Vec<Fingerprint> {
        seeds
            .iter()
            .map(|seed| utils::compute_fingerprint(&key_of(*seed)))
            .collect()
    }

    /// p2 rotates its key 2 for key 9 on a party that p1, p2 and p3 host.
    fn claims_check(proposed: &[u8], claims: &[(&str, u8)]) -> Result<Fingerprint> {
        let members = ["p1", "p2", "p3"].map(str::to_string);
        check_rotation_claims(
            "p2",
            &utils::compute_fingerprint(&key_of(2)),
            &members,
            &fingerprints(proposed),
            &member_claims(claims),
        )
    }

    #[test]
    fn a_rotation_returns_the_one_key_no_other_member_holds() -> Result {
        let new_key = claims_check(&[1, 9, 3], &[("p1", 1), ("p2", 2), ("p3", 3)])?;
        assert_eq!(new_key, utils::compute_fingerprint(&key_of(9)));
        // A node that already recorded the new key accepts the same proposal.
        let again = claims_check(&[1, 9, 3], &[("p1", 1), ("p2", 9), ("p3", 3)])?;
        assert_eq!(again, new_key);
        Ok(())
    }

    /// Each refusal closes a way for a coordinator to take another member's
    /// place, or to keep a key that no current member holds.
    #[test]
    fn a_rotation_is_refused_unless_every_other_key_is_accounted_for() {
        for (proposed, claims, expected) in [
            (
                &[1u8, 9, 3][..],
                &[("p1", 1u8), ("p2", 2)][..],
                "no Daml key on record for p3",
            ),
            (
                &[1, 9, 3],
                &[("p1", 2), ("p2", 2), ("p3", 3)],
                "on record as p1's",
            ),
            (&[9, 3, 4], &[("p1", 1), ("p2", 2), ("p3", 3)], "drops"),
            (
                &[1, 9, 3, 4],
                &[("p1", 1), ("p2", 2), ("p3", 3)],
                "leaves 2 keys",
            ),
            (
                &[1, 9, 3],
                &[("p1", 3), ("p2", 2), ("p3", 3)],
                "on record for both",
            ),
            (
                &[1, 9, 3],
                &[("p1", 1), ("p2", 2), ("p3", 3), ("p4", 9)],
                "on record as p4's",
            ),
            (
                &[1, 9, 3],
                &[("p1", 1), ("p2", 5), ("p3", 3)],
                "retry that run",
            ),
            (
                &[1, 9, 3],
                &[("p1", 1), ("p3", 3)],
                "no Daml key on record for p2",
            ),
        ] {
            let error = match claims_check(proposed, claims) {
                Ok(key) => panic!("expected a refusal for {claims:?}, got {key}"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains(expected),
                "{claims:?}: expected {expected:?} in: {error}"
            );
        }
    }
}
