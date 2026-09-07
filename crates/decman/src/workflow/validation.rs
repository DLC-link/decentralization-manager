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

use std::collections::BTreeSet;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningPublicKey,
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
        TopologyTransaction, enums, topology_mapping,
    },
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    server::WorkflowKind,
    utils,
    workflow::{
        onboarding::steps::proposals::create::{
            compute_decentralized_namespace, decode_keys_payload,
        },
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
    },
};

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
    /// The full member set: the participants named in the invitation plus the
    /// coordinator itself (onboarding and kick invitations list only the
    /// invitees; add-party and change-threshold already include the
    /// coordinator, so the union is the same set either way).
    pub members: BTreeSet<CantonId>,
    /// The dec party the run targets. Absent for onboarding, where the party
    /// does not exist yet.
    pub dec_party_id: Option<CantonId>,
    /// Onboarding only: the party id prefix the coordinator proposed.
    pub prefix: Option<String>,
    /// The threshold the topology should end up with.
    pub threshold: Option<i32>,
    /// Kick only: the participant being removed.
    pub kicked_participant: Option<CantonId>,
    /// Add-party only: the participant being added.
    pub new_participant: Option<CantonId>,
    /// Dars only: the filenames the operator agreed to install.
    pub dar_filenames: Vec<String>,
    /// Dars only: SHA-256 of each accepted DAR, index-aligned with
    /// `dar_filenames`. Empty when the coordinator predates the field.
    pub dar_hashes: Vec<String>,
    /// This node's own participant id.
    pub self_id: CantonId,
}

/// The invitation fields `insert_peer_run` stores on the run row.
#[derive(serde::Deserialize)]
struct PeerRunConfig {
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
    #[serde(default)]
    new_participant_id: Option<CantonId>,
    #[serde(default)]
    new_threshold: Option<i32>,
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

        Ok(Self {
            kind: run.kind,
            members,
            dec_party_id: run.dec_party_id,
            prefix: config.prefix,
            threshold: config.new_threshold,
            kicked_participant: config.participant_id,
            new_participant: config.new_participant_id,
            dar_filenames: config.dar_filenames,
            dar_hashes: config.dar_hashes,
            self_id: node_config.participant_id().clone(),
        })
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

        let own_namespace = self.own_namespace(storage, instance_name).await?;
        if !owners.contains(&own_namespace) {
            anyhow::bail!(
                "DNS proposal does not include this node's namespace {own_namespace} \
                 among its owners"
            );
        }

        self.check_threshold(namespace_def.threshold, owners.len())?;

        let owner_set: std::collections::HashSet<String> = owners.iter().cloned().collect();
        let computed = compute_decentralized_namespace(&owner_set);
        if computed != namespace_def.decentralized_namespace {
            anyhow::bail!(
                "DNS proposal claims namespace {claimed} but its owner set hashes to \
                 {computed}",
                claimed = namespace_def.decentralized_namespace
            );
        }

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

        match dns_namespace {
            Some(namespace) if namespace != party.namespace.to_hex() => anyhow::bail!(
                "P2P proposal targets namespace {found} but this node signed DNS for \
                 {namespace}",
                found = party.namespace.to_hex()
            ),
            Some(_) => {}
            None => tracing::warn!(
                "no signed DNS namespace on record for {instance_name}; skipping the \
                 namespace cross-check on the P2P proposal"
            ),
        }

        self.check_p2p_membership(&mapping)?;
        self.check_onboarding_markers(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;
        self.check_own_daml_key(storage, instance_name, &mapping)
            .await
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
        if owners.len() != self.members.len() {
            anyhow::bail!(
                "DNS proposal has {found} owners but the accepted invitation names \
                 {expected} members",
                found = owners.len(),
                expected = self.members.len()
            );
        }
        self.check_threshold(namespace_def.threshold, owners.len())?;

        // Losing our namespace from the owner set would leave this node hosting
        // a party it can no longer authorize changes to. The lookup itself is
        // best-effort — a party onboarded before the identity table existed may
        // have no local record — but a namespace we *can* resolve must be there.
        match self.own_namespace(storage, instance_name).await {
            Ok(own_namespace) if !namespace_def.owners.contains(&own_namespace) => anyhow::bail!(
                "DNS proposal drops this node's namespace {own_namespace} from the owner set"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                "cannot resolve this node's namespace ({e}); skipping the owner-set \
                 membership check on the DNS proposal"
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

        tracing::info!(
            "{kind:?} proposals match the accepted invitation for {dec_party_id}",
            kind = self.kind
        );
        Ok(())
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
    /// threshold unchecked would let a coordinator raise a hosting threshold
    /// the operator agreed to while quietly dropping the signing one to 1.
    fn check_p2p_thresholds(&self, mapping: &PartyToParticipant) -> Result {
        self.check_threshold(i32::try_from(mapping.threshold)?, self.members.len())?;
        match &mapping.party_signing_keys {
            Some(signing_keys) => {
                self.check_threshold(i32::try_from(signing_keys.threshold)?, self.members.len())
            }
            // Signing a mapping with no party signing keys does not merely
            // leave a threshold unchecked — it submits a party that can no
            // longer authorize anything. Every mapping this tool produces
            // carries them (onboarding sets them, and the kick / add-party /
            // change-threshold / clearing proposals all derive from the current
            // on-chain mapping), so an absent set is a stripped proposal.
            None => anyhow::bail!(
                "P2P proposal carries no party signing keys, which would leave the party \
                 unable to authorize anything"
            ),
        }
    }

    /// The threshold must be the one the invitation advertised, and in any
    /// case a sane value for the owner count — a threshold of 0 or one above
    /// the owner count would either need no signatures or deadlock the party.
    fn check_threshold(&self, threshold: i32, owner_count: usize) -> Result {
        let owner_count = i32::try_from(owner_count)?;
        if !(1..=owner_count).contains(&threshold) {
            anyhow::bail!("proposed threshold {threshold} is outside 1..={owner_count}");
        }
        match self.threshold {
            Some(expected) if expected != threshold => {
                anyhow::bail!("proposed threshold {threshold} differs from the accepted {expected}")
            }
            Some(_) => Ok(()),
            // Only the onboarding invite's threshold is optional on the wire
            // (`Option<i32>`, absent from coordinators that predate it). Kick,
            // add-party and change-threshold all require it, so an absent one
            // there would let the coordinator pick any in-range value.
            None if self.kind == WorkflowKind::Onboarding => {
                tracing::warn!(
                    "accepted onboarding invitation carried no threshold; only the range \
                     check was applied to the proposed {threshold}"
                );
                Ok(())
            }
            None => anyhow::bail!(
                "accepted {kind:?} invitation carries no threshold, so the proposed \
                 {threshold} cannot be checked against it",
                kind = self.kind
            ),
        }
    }

    /// This node's Daml signing key must be among the party's signing keys, or
    /// it would be a member that cannot sign for the party it just authorized.
    async fn check_own_daml_key(
        &self,
        storage: &SqlitePool,
        instance_name: &str,
        mapping: &PartyToParticipant,
    ) -> Result {
        let keys = self.own_keys(storage, instance_name).await?;
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

    /// This node's namespace fingerprint for the run: from the keys it
    /// generated during onboarding, falling back to the long-lived identity
    /// row when the run's artefacts are already gone.
    async fn own_namespace(&self, storage: &SqlitePool, instance_name: &str) -> Result<String> {
        let keys = self.own_keys(storage, instance_name).await?;
        Ok(utils::compute_fingerprint(&keys[0]))
    }

    /// This node's `[namespace_key, daml_key]` bundle.
    async fn own_keys(
        &self,
        storage: &SqlitePool,
        instance_name: &str,
    ) -> Result<Vec<SigningPublicKey>> {
        let self_id = self.self_id.to_string();
        let payload = match storage
            .read_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&self_id),
            )
            .await?
        {
            Some(payload) => payload,
            None => {
                let dec_party_id = self.dec_party_id.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "this node's public keys are not on the {instance_name} run and \
                         there is no dec party to look them up under"
                    )
                })?;
                storage
                    .read_identity(dec_party_id, identity_kinds::PEER_PUBLIC_KEYS, &self_id)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "this node's public keys are unknown for {dec_party_id}, so the \
                             proposal cannot be checked against them"
                        )
                    })?
            }
        };

        let keys = decode_keys_payload(&payload)?;
        if keys.len() != 2 {
            anyhow::bail!(
                "expected 2 public keys for this node, found {count}",
                count = keys.len()
            );
        }
        Ok(keys)
    }
}

/// Unwrap a coordinator-supplied `varint(len)||SignedTopologyTransaction`
/// blob down to the topology mapping it carries.
///
/// Only an add-or-replace is ever signed. A `Remove` carrying the right party
/// and the right members would pass every other check while tearing the
/// mapping down, so the operation is pinned here, before anything else looks
/// at the mapping.
fn decode_topology_mapping(payload: &[u8]) -> Result<topology_mapping::Mapping> {
    let signed: SignedTopologyTransaction = utils::read_first_message_from_bytes(payload)?;
    let transaction = utils::decode_versioned::<TopologyTransaction>(&signed.transaction)?;

    if transaction.operation != enums::TopologyChangeOp::AddReplace as i32 {
        anyhow::bail!(
            "topology transaction is a {operation:?} operation, not an add-or-replace",
            operation = enums::TopologyChangeOp::try_from(transaction.operation)
                .map(|op| op.as_str_name().to_string())
                .unwrap_or_else(|_| format!("unknown ({})", transaction.operation))
        );
    }

    transaction
        .mapping
        .and_then(|mapping| mapping.mapping)
        .ok_or_else(|| anyhow::anyhow!("topology transaction carries no mapping"))
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
            members: members.into_iter().collect(),
            dec_party_id: None,
            prefix: None,
            threshold: Some(2),
            kicked_participant: None,
            new_participant: None,
            dar_filenames: Vec::new(),
            dar_hashes: Vec::new(),
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
        assert!(expectations.check_threshold(1, 3).is_err());
        Ok(())
    }

    /// A threshold of zero would let any single signer act for the party.
    #[test]
    fn rejects_an_out_of_range_threshold() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.threshold = Some(0);
        assert!(expectations.check_threshold(0, 3).is_err());
        expectations.threshold = Some(4);
        assert!(expectations.check_threshold(4, 3).is_err());
        Ok(())
    }

    #[test]
    fn accepts_the_advertised_threshold() -> Result {
        let a = canton_id("p1", 1)?;
        let expectations = expectations(vec![a.clone()], a);
        expectations.check_threshold(2, 3)
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
                .check_party_proposals(&pool, "run", &dns, &p2p_payload)
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
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: Vec::new(),
            threshold: 1,
        });
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
        assert!(expectations.check_threshold(2, 3).is_err());
        expectations.kind = WorkflowKind::AddParty;
        assert!(expectations.check_threshold(2, 3).is_err());

        expectations.kind = WorkflowKind::Onboarding;
        expectations.check_threshold(2, 3)
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
                .check_onboarding_p2p(&pool, "run", &payload, None)
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
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: Vec::new(),
            threshold: 2,
        });
        expectations.check_p2p_thresholds(&mapping)
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
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: Vec::new(),
            threshold: 2,
        });
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
            .check_party_proposals(&pool, "run", &dns, &p2p_payload)
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
}
