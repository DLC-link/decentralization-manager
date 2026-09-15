//! What a member checks before it co-signs a topology proposal (design
//! section 5).
//!
//! The operator accepts a `WorkflowProposal` once. After that the node signs
//! only proposals that match that record, its counted acceptances, the head
//! state, and this node's own identity. Every check here is a pure function
//! over those inputs, so each rule has a unit test and no ledger is needed.
//!
//! Hosts compare as full `(participant_uid, permission, onboarding)` tuples.
//! Keys compare as full serialized `SigningPublicKey` byte sets. A mismatch
//! is an error the caller shows in the UI; the node never signs it.

use std::collections::{BTreeMap, BTreeSet};

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningPublicKey,
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, enums::ParticipantPermission,
    },
};
use common::{canton_id::CantonId, types::WorkflowKind};
use prost::Message;

use crate::utils;

use super::{
    daml::codec::WorkflowProposalRecord,
    proposals::Acceptance,
    topology::{PendingProposal, compute_namespace},
};

/// Why a proposal must not be signed. The text goes to the run row and the
/// UI unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError(pub String);

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ValidationError {}

pub type Check = Result<(), ValidationError>;

macro_rules! refuse {
    ($($arg:tt)*) => {
        return Err(ValidationError(format!($($arg)*)))
    };
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// A host row as a comparable tuple.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostTuple {
    pub uid: String,
    pub permission: i32,
    pub onboarding: bool,
}

impl HostTuple {
    pub fn confirmation(uid: &str, onboarding: bool) -> Self {
        Self {
            uid: uid.to_string(),
            permission: ParticipantPermission::Confirmation as i32,
            onboarding,
        }
    }
}

/// The host set of a mapping, as tuples.
pub fn host_tuples(mapping: &PartyToParticipant) -> BTreeSet<HostTuple> {
    mapping
        .participants
        .iter()
        .map(|h| HostTuple {
            uid: h.participant_uid.clone(),
            permission: h.permission,
            onboarding: h.onboarding.is_some(),
        })
        .collect()
}

/// The key set of a mapping, as serialized bytes.
pub fn key_bytes(mapping: &PartyToParticipant) -> BTreeSet<Vec<u8>> {
    mapping
        .party_signing_keys
        .as_ref()
        .map(|k| k.keys.iter().map(Message::encode_to_vec).collect())
        .unwrap_or_default()
}

/// The key set of a mapping, as fingerprints.
pub fn key_fingerprints(mapping: &PartyToParticipant) -> BTreeSet<String> {
    mapping
        .party_signing_keys
        .as_ref()
        .map(|k| k.keys.iter().map(utils::compute_fingerprint).collect())
        .unwrap_or_default()
}

fn signing_threshold(mapping: &PartyToParticipant) -> Option<u32> {
    mapping.party_signing_keys.as_ref().map(|k| k.threshold)
}

/// This node, as the proposal must name it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalIdentity {
    pub participant_id: CantonId,
    /// Namespace fingerprints this node owns for the party: the dual-usage
    /// key of a new party, or the legacy `{prefix}-namespace` key.
    pub owner_fingerprints: BTreeSet<String>,
    /// This node's Daml signing-key fingerprint for the party, when known.
    pub daml_key_fingerprint: Option<String>,
}

/// The member a kick removes, from this node's own cache
/// (`dec_party_participant.owner_key` / `signing_key`), never from the
/// proposer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KickedMember {
    pub participant_id: String,
    pub owner_fingerprint: String,
    pub signing_key_fingerprint: Option<String>,
}

/// The fields of a counted acceptance the checks use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CountedAcceptance {
    pub acceptor: CantonId,
    pub participant_id: String,
    pub namespace_fingerprint: Option<String>,
    pub signing_public_key_hex: Option<String>,
    pub daml_key_fingerprint: Option<String>,
}

impl From<&Acceptance> for CountedAcceptance {
    fn from(a: &Acceptance) -> Self {
        Self {
            acceptor: a.record.acceptor.clone(),
            participant_id: a.record.participant_id.clone(),
            namespace_fingerprint: a.record.namespace_fingerprint.clone(),
            signing_public_key_hex: a.record.signing_public_key_hex.clone(),
            daml_key_fingerprint: a.record.daml_key_fingerprint.clone(),
        }
    }
}

/// The accepted head mappings of the party, when it exists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeadState {
    pub dnd: Option<DecentralizedNamespaceDefinition>,
    pub p2p: Option<PartyToParticipant>,
}

/// The reference set a member validates against.
#[derive(Clone, Debug, PartialEq)]
pub struct Expectations {
    pub kind: WorkflowKind,
    pub proposer_participant: String,
    /// The proposer's owner fingerprint. `None` fails every check closed.
    pub proposer_fingerprint: Option<String>,
    /// Whether `proposer_participant` hosts the proposer node party with
    /// Submission (design D2), read by the caller.
    pub proposer_hosting_ok: bool,
    pub participants: Vec<String>,
    pub prefix: Option<String>,
    pub dec_party_id: Option<String>,
    pub threshold: Option<u32>,
    pub previous_threshold: Option<u32>,
    pub dnd_base_serial: Option<u32>,
    pub p2p_base_serial: Option<u32>,
    pub new_participant: Option<String>,
    pub kicked_participant: Option<String>,
    pub accepted: Vec<CountedAcceptance>,
    pub head: HeadState,
    /// Root-NSD `target_key` per owner fingerprint, read from the
    /// synchronizer store (design D4).
    pub owner_keys: BTreeMap<String, SigningPublicKey>,
    pub identity: LocalIdentity,
    pub kicked: Option<KickedMember>,
    /// Daml key fingerprint claimed by each surviving member
    /// (`participant_uid -> fingerprint`), for the kick legacy branch.
    pub survivor_key_claims: BTreeMap<String, String>,
}

fn to_u32(v: Option<i64>) -> Option<u32> {
    v.and_then(|n| u32::try_from(n).ok())
}

impl Expectations {
    /// Build the reference set from the accepted proposal, its counted
    /// acceptances, the head state, and this node's identity. Optional
    /// inputs (root keys, kicked member, survivor claims, hosting check)
    /// are added with the `with_*` methods.
    pub fn new(
        proposal: &WorkflowProposalRecord,
        counted: &[Acceptance],
        head: HeadState,
        identity: LocalIdentity,
    ) -> Self {
        Self {
            kind: proposal.kind,
            proposer_participant: proposal.proposer_participant.clone(),
            proposer_fingerprint: proposal.proposer_namespace_fingerprint.clone(),
            proposer_hosting_ok: false,
            participants: proposal.participants.clone(),
            prefix: proposal.prefix.clone(),
            dec_party_id: proposal.dec_party_id.clone(),
            threshold: to_u32(proposal.threshold),
            previous_threshold: to_u32(proposal.previous_threshold),
            dnd_base_serial: to_u32(proposal.dnd_base_serial),
            p2p_base_serial: to_u32(proposal.p2p_base_serial),
            new_participant: proposal.new_participant.clone(),
            kicked_participant: proposal.kicked_participant.clone(),
            accepted: counted.iter().map(CountedAcceptance::from).collect(),
            head,
            owner_keys: BTreeMap::new(),
            identity,
            kicked: None,
            survivor_key_claims: BTreeMap::new(),
        }
    }

    pub fn with_owner_keys(mut self, keys: BTreeMap<String, SigningPublicKey>) -> Self {
        self.owner_keys = keys;
        self
    }

    pub fn with_kicked(mut self, kicked: KickedMember) -> Self {
        self.kicked = Some(kicked);
        self
    }

    pub fn with_survivor_key_claims(mut self, claims: BTreeMap<String, String>) -> Self {
        self.survivor_key_claims = claims;
        self
    }

    pub fn with_proposer_hosting(mut self, ok: bool) -> Self {
        self.proposer_hosting_ok = ok;
        self
    }

    /// The threshold the proposal advertised. `None` fails closed.
    pub fn required_threshold(&self) -> Result<u32, ValidationError> {
        self.threshold
            .ok_or_else(|| ValidationError("the accepted proposal carries no threshold".into()))
    }

    /// The proposer's owner fingerprint. `None` fails closed.
    pub fn required_proposer_fingerprint(&self) -> Result<&str, ValidationError> {
        self.proposer_fingerprint.as_deref().ok_or_else(|| {
            ValidationError(
                "the accepted proposal carries no proposer namespace fingerprint".into(),
            )
        })
    }

    /// Onboarding owners: the proposer plus every counted acceptor.
    pub fn onboarding_owners(&self) -> Result<BTreeSet<String>, ValidationError> {
        let mut owners = BTreeSet::new();
        owners.insert(self.required_proposer_fingerprint()?.to_string());
        for a in &self.accepted {
            let Some(fp) = &a.namespace_fingerprint else {
                refuse!(
                    "acceptance from {} on {} carries no namespace fingerprint",
                    a.acceptor,
                    a.participant_id
                );
            };
            if !owners.insert(fp.clone()) {
                refuse!("owner fingerprint {fp} appears twice among the acceptances");
            }
        }
        Ok(owners)
    }

    /// Onboarding hosts: the proposer's participant plus every counted
    /// acceptor's participant, all at Confirmation without the marker.
    pub fn onboarding_hosts(&self) -> BTreeSet<HostTuple> {
        let mut hosts = BTreeSet::new();
        hosts.insert(HostTuple::confirmation(&self.proposer_participant, false));
        for a in &self.accepted {
            hosts.insert(HostTuple::confirmation(&a.participant_id, false));
        }
        hosts
    }

    /// The counted acceptance of the add-party joiner.
    pub fn joiner(&self) -> Result<&CountedAcceptance, ValidationError> {
        let Some(new_participant) = &self.new_participant else {
            refuse!("the accepted add-party proposal names no new participant");
        };
        self.accepted
            .iter()
            .find(|a| &a.participant_id == new_participant)
            .ok_or_else(|| {
                ValidationError(format!(
                    "no counted acceptance from the joiner {new_participant} yet"
                ))
            })
    }

    fn head_dnd(&self) -> Result<&DecentralizedNamespaceDefinition, ValidationError> {
        self.head
            .dnd
            .as_ref()
            .ok_or_else(|| ValidationError("no accepted DND in the head state".into()))
    }

    fn head_p2p(&self) -> Result<&PartyToParticipant, ValidationError> {
        self.head
            .p2p
            .as_ref()
            .ok_or_else(|| ValidationError("no accepted P2P in the head state".into()))
    }

    fn own_participant(&self) -> String {
        self.identity.participant_id.to_string()
    }
}

// ---------------------------------------------------------------------------
// Common checks
// ---------------------------------------------------------------------------

/// The serial guard inputs: the accepted serial read this tick and the base
/// serial the `WorkflowProposal` recorded.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SerialContext {
    pub accepted: Option<u32>,
    pub base: Option<u32>,
}

/// Design section 5, "Common": `ADD_REPLACE`; the proposer's owner
/// fingerprint has signed; this node has not; `serial == accepted + 1` and
/// `accepted == base` (or `1` with no accepted mapping).
pub fn check_common<M>(
    pending: &PendingProposal<M>,
    exp: &Expectations,
    serials: &SerialContext,
) -> Check {
    if !pending.is_add_replace() {
        refuse!(
            "proposal {} is not an ADD_REPLACE (operation {}); a REMOVE is never co-signed",
            pending.hash_hex,
            pending.operation
        );
    }
    let proposer = exp.required_proposer_fingerprint()?;
    if !pending.is_signed_by(proposer) {
        refuse!(
            "proposal {} is not signed by the proposer's owner key {proposer}",
            pending.hash_hex
        );
    }
    if let Some(own) = exp
        .identity
        .owner_fingerprints
        .iter()
        .find(|fp| pending.is_signed_by(fp))
    {
        refuse!(
            "proposal {} already carries this node's signature {own}",
            pending.hash_hex
        );
    }
    match serials.accepted {
        None => {
            if serials.base.is_some_and(|b| b != 0) {
                refuse!(
                    "no accepted mapping exists but the proposal recorded base serial {:?}",
                    serials.base
                );
            }
            if pending.serial != 1 {
                refuse!(
                    "first mapping must be proposed at serial 1, found {}",
                    pending.serial
                );
            }
        }
        Some(accepted) => {
            if serials.base != Some(accepted) {
                refuse!(
                    "topology moved: accepted serial is {accepted} but the proposal recorded \
                     base serial {:?}",
                    serials.base
                );
            }
            if pending.serial != accepted + 1 {
                refuse!(
                    "proposal serial {} is not accepted serial {accepted} + 1",
                    pending.serial
                );
            }
        }
    }
    Ok(())
}

/// DND rule: this node's owner fingerprint is among the owners.
pub fn check_own_owner(dnd: &DecentralizedNamespaceDefinition, exp: &Expectations) -> Check {
    if exp.identity.owner_fingerprints.is_empty() {
        refuse!("this node has no owner fingerprint for the party");
    }
    if !exp
        .identity
        .owner_fingerprints
        .iter()
        .any(|fp| dnd.owners.contains(fp))
    {
        refuse!(
            "DND owners {:?} do not include this node's fingerprint(s) {:?}",
            dnd.owners,
            exp.identity.owner_fingerprints
        );
    }
    Ok(())
}

/// P2P rule: this node's participant is a host at Confirmation. The
/// Onboarding marker is allowed only when `as_joiner` is set.
pub fn check_own_host(p2p: &PartyToParticipant, exp: &Expectations, as_joiner: bool) -> Check {
    let own = exp.own_participant();
    let Some(host) = p2p.participants.iter().find(|h| h.participant_uid == own) else {
        refuse!("P2P does not host this node ({own})");
    };
    if host.permission != ParticipantPermission::Confirmation as i32 {
        refuse!(
            "P2P hosts this node ({own}) with permission {}, not Confirmation",
            host.permission
        );
    }
    if host.onboarding.is_some() != as_joiner {
        refuse!(
            "P2P marks this node ({own}) as onboarding: {}, expected {as_joiner}",
            host.onboarding.is_some()
        );
    }
    Ok(())
}

fn check_dnd_threshold(dnd: &DecentralizedNamespaceDefinition, exp: &Expectations) -> Check {
    let expected = exp.required_threshold()?;
    let owners = u32::try_from(dnd.owners.len()).unwrap_or(u32::MAX);
    if dnd.threshold < 1 || u32::try_from(dnd.threshold).unwrap_or(0) > owners {
        refuse!("DND threshold {} is outside 1..={owners}", dnd.threshold);
    }
    if u32::try_from(dnd.threshold).ok() != Some(expected) {
        refuse!(
            "DND threshold {} differs from the accepted {expected}",
            dnd.threshold
        );
    }
    Ok(())
}

fn check_p2p_thresholds(p2p: &PartyToParticipant, exp: &Expectations) -> Check {
    let expected = exp.required_threshold()?;
    let hosts = u32::try_from(p2p.participants.len()).unwrap_or(u32::MAX);
    if p2p.threshold < 1 || p2p.threshold > hosts {
        refuse!(
            "P2P hosting threshold {} is outside 1..={hosts}",
            p2p.threshold
        );
    }
    if p2p.threshold != expected {
        refuse!(
            "P2P hosting threshold {} differs from the accepted {expected}",
            p2p.threshold
        );
    }
    let Some(signing) = signing_threshold(p2p) else {
        refuse!("P2P carries no party signing keys, which would leave the party unable to sign");
    };
    if signing != expected {
        refuse!("P2P signing threshold {signing} differs from the accepted {expected}");
    }
    let keys = u32::try_from(key_bytes(p2p).len()).unwrap_or(u32::MAX);
    if signing < 1 || signing > keys {
        refuse!("P2P signing threshold {signing} is outside 1..={keys}");
    }
    Ok(())
}

fn owner_key_bytes(
    exp: &Expectations,
    owners: &BTreeSet<String>,
) -> Result<BTreeSet<Vec<u8>>, ValidationError> {
    let mut out = BTreeSet::new();
    for fp in owners {
        let Some(key) = exp.owner_keys.get(fp) else {
            refuse!("no root NamespaceDelegation key for owner {fp}");
        };
        out.insert(key.encode_to_vec());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Onboarding
// ---------------------------------------------------------------------------

/// Onboarding DND: owners are exactly the proposer plus the counted
/// acceptors, one per participant; threshold as accepted; namespace derived
/// from the owners; this node among the owners.
pub fn check_onboarding_dnd(
    pending: &PendingProposal<DecentralizedNamespaceDefinition>,
    exp: &Expectations,
) -> Check {
    let dnd = &pending.mapping;
    let expected = exp.onboarding_owners()?;
    let proposed: BTreeSet<String> = dnd.owners.iter().cloned().collect();
    if proposed.len() != dnd.owners.len() {
        refuse!("DND repeats an owner");
    }
    if proposed != expected {
        refuse!(
            "DND owners {:?} differ from the accepted set {:?}",
            proposed,
            expected
        );
    }
    if proposed.len() != exp.participants.len() {
        refuse!(
            "DND has {} owners for {} participants",
            proposed.len(),
            exp.participants.len()
        );
    }
    check_dnd_threshold(dnd, exp)?;
    let namespace = compute_namespace(proposed.iter());
    if dnd.decentralized_namespace != namespace {
        refuse!(
            "DND namespace {} is not derived from its owners ({namespace})",
            dnd.decentralized_namespace
        );
    }
    check_own_owner(dnd, exp)
}

/// Onboarding P2P: party is `prefix::namespace`; hosts are exactly the
/// proposer's and acceptors' participants at Confirmation without marker;
/// the proposer is hosted where it claims; keys are exactly the root-NSD
/// keys of the owners; both thresholds as accepted; this node hosts.
pub fn check_onboarding_p2p(
    pending: &PendingProposal<PartyToParticipant>,
    exp: &Expectations,
) -> Check {
    let p2p = &pending.mapping;
    let owners = exp.onboarding_owners()?;
    let Some(prefix) = &exp.prefix else {
        refuse!("the accepted onboarding proposal carries no prefix");
    };
    let party = format!("{prefix}::{}", compute_namespace(owners.iter()));
    if p2p.party != party {
        refuse!("P2P party {} is not the accepted {party}", p2p.party);
    }
    let hosts = host_tuples(p2p);
    if hosts.len() != p2p.participants.len() {
        refuse!("P2P lists a participant twice");
    }
    let expected_hosts = exp.onboarding_hosts();
    if hosts != expected_hosts {
        refuse!(
            "P2P hosts {:?} differ from the accepted set {:?}",
            hosts,
            expected_hosts
        );
    }
    if !exp.proposer_hosting_ok {
        refuse!(
            "the proposer node party is not hosted on {} with Submission",
            exp.proposer_participant
        );
    }
    let expected_keys = owner_key_bytes(exp, &owners)?;
    if key_bytes(p2p) != expected_keys {
        refuse!("P2P party signing keys differ from the owners' root delegation keys");
    }
    check_p2p_thresholds(p2p, exp)?;
    check_own_host(p2p, exp, false)
}

// ---------------------------------------------------------------------------
// Add party
// ---------------------------------------------------------------------------

/// Add-party DND: head owners plus the joiner; threshold as accepted; this
/// node among the owners.
pub fn check_add_party_dnd(
    pending: &PendingProposal<DecentralizedNamespaceDefinition>,
    exp: &Expectations,
) -> Check {
    let dnd = &pending.mapping;
    let head = exp.head_dnd()?;
    let joiner = exp.joiner()?;
    let Some(joiner_fp) = &joiner.namespace_fingerprint else {
        refuse!("the joiner's acceptance carries no namespace fingerprint");
    };
    let mut expected: BTreeSet<String> = head.owners.iter().cloned().collect();
    if !expected.insert(joiner_fp.clone()) {
        refuse!("joiner fingerprint {joiner_fp} is already an owner");
    }
    let proposed: BTreeSet<String> = dnd.owners.iter().cloned().collect();
    if proposed.len() != dnd.owners.len() {
        refuse!("DND repeats an owner");
    }
    if proposed != expected {
        refuse!(
            "DND owners {:?} are not the head owners plus the joiner {:?}",
            proposed,
            expected
        );
    }
    if dnd.decentralized_namespace != head.decentralized_namespace {
        refuse!(
            "DND namespace {} differs from the head {}",
            dnd.decentralized_namespace,
            head.decentralized_namespace
        );
    }
    check_dnd_threshold(dnd, exp)?;
    check_own_owner(dnd, exp)
}

/// Add-party P2P: head hosts unchanged plus the joiner at Confirmation with
/// the Onboarding marker; head keys plus the joiner's root-NSD key; both
/// thresholds as accepted; this node hosts (or is the joiner).
pub fn check_add_party_p2p(
    pending: &PendingProposal<PartyToParticipant>,
    exp: &Expectations,
) -> Check {
    let p2p = &pending.mapping;
    let head = exp.head_p2p()?;
    let joiner = exp.joiner()?;
    if p2p.party != head.party {
        refuse!(
            "P2P party {} differs from the head {}",
            p2p.party,
            head.party
        );
    }
    let hosts = host_tuples(p2p);
    if hosts.len() != p2p.participants.len() {
        refuse!("P2P lists a participant twice");
    }
    let mut expected = host_tuples(head);
    if expected.iter().any(|h| h.uid == joiner.participant_id) {
        refuse!("joiner {} already hosts the party", joiner.participant_id);
    }
    expected.insert(HostTuple::confirmation(&joiner.participant_id, true));
    if hosts != expected {
        refuse!(
            "P2P hosts {:?} are not the head hosts plus the onboarding joiner {:?}",
            hosts,
            expected
        );
    }
    let Some(joiner_fp) = &joiner.namespace_fingerprint else {
        refuse!("the joiner's acceptance carries no namespace fingerprint");
    };
    let Some(joiner_key) = exp.owner_keys.get(joiner_fp) else {
        refuse!("no root NamespaceDelegation key for the joiner {joiner_fp}");
    };
    let mut expected_keys = key_bytes(head);
    expected_keys.insert(joiner_key.encode_to_vec());
    if key_bytes(p2p) != expected_keys {
        refuse!("P2P party signing keys are not the head keys plus the joiner's root key");
    }
    check_p2p_thresholds(p2p, exp)?;
    let as_joiner = exp.own_participant() == joiner.participant_id;
    check_own_host(p2p, exp, as_joiner)
}

// ---------------------------------------------------------------------------
// Kick
// ---------------------------------------------------------------------------

/// Kick DND: head owners minus the kicked fingerprint from this node's own
/// cache; exactly one fewer owner; threshold as accepted; this node among
/// the owners.
pub fn check_kick_dnd(
    pending: &PendingProposal<DecentralizedNamespaceDefinition>,
    exp: &Expectations,
) -> Check {
    let dnd = &pending.mapping;
    let head = exp.head_dnd()?;
    let Some(kicked) = &exp.kicked else {
        refuse!("this node has no cached owner key for the kicked participant");
    };
    if !head.owners.contains(&kicked.owner_fingerprint) {
        refuse!(
            "cached owner key {} of the kicked participant is not a head owner",
            kicked.owner_fingerprint
        );
    }
    let expected: BTreeSet<String> = head
        .owners
        .iter()
        .filter(|o| **o != kicked.owner_fingerprint)
        .cloned()
        .collect();
    let proposed: BTreeSet<String> = dnd.owners.iter().cloned().collect();
    if proposed.len() != dnd.owners.len() {
        refuse!("DND repeats an owner");
    }
    if proposed != expected {
        refuse!(
            "DND owners {:?} are not the head owners minus the kicked {} ({:?})",
            proposed,
            kicked.owner_fingerprint,
            expected
        );
    }
    if proposed.len() + 1 != head.owners.len() {
        refuse!(
            "DND has {} owners; the head has {}",
            proposed.len(),
            head.owners.len()
        );
    }
    if dnd.decentralized_namespace != head.decentralized_namespace {
        refuse!(
            "DND namespace {} differs from the head {}",
            dnd.decentralized_namespace,
            head.decentralized_namespace
        );
    }
    check_dnd_threshold(dnd, exp)?;
    check_own_owner(dnd, exp)
}

/// Kick P2P: head hosts minus the kicked participant, survivors unchanged;
/// exactly one key removed, which is not this node's and not claimed by a
/// survivor; both thresholds as accepted; this node hosts.
///
/// Dual-key parties (party signing key fingerprints equal the DND owners)
/// pin the removed key to the kicked owner fingerprint. Legacy parties use
/// the elimination rule alone.
pub fn check_kick_p2p(pending: &PendingProposal<PartyToParticipant>, exp: &Expectations) -> Check {
    let p2p = &pending.mapping;
    let head = exp.head_p2p()?;
    let Some(kicked) = &exp.kicked else {
        refuse!("this node has no cached owner key for the kicked participant");
    };
    if p2p.party != head.party {
        refuse!(
            "P2P party {} differs from the head {}",
            p2p.party,
            head.party
        );
    }
    let hosts = host_tuples(p2p);
    if hosts.len() != p2p.participants.len() {
        refuse!("P2P lists a participant twice");
    }
    let head_hosts = host_tuples(head);
    if !head_hosts.iter().any(|h| h.uid == kicked.participant_id) {
        refuse!(
            "kicked participant {} does not host the party",
            kicked.participant_id
        );
    }
    let expected: BTreeSet<HostTuple> = head_hosts
        .into_iter()
        .filter(|h| h.uid != kicked.participant_id)
        .collect();
    if hosts != expected {
        refuse!(
            "P2P hosts {:?} are not the head hosts minus the kicked {} ({:?})",
            hosts,
            kicked.participant_id,
            expected
        );
    }

    let head_keys = key_fingerprints(head);
    let proposed_keys = key_fingerprints(p2p);
    if !proposed_keys.is_subset(&head_keys) {
        refuse!("P2P adds a party signing key during a kick");
    }
    let removed: Vec<&String> = head_keys.difference(&proposed_keys).collect();
    let [removed] = removed.as_slice() else {
        refuse!(
            "P2P removes {} party signing key(s); a kick removes exactly one",
            removed.len()
        );
    };
    if exp.identity.daml_key_fingerprint.as_deref() == Some(removed.as_str()) {
        refuse!("P2P removes this node's own Daml signing key {removed}");
    }
    if let Some((survivor, _)) = exp
        .survivor_key_claims
        .iter()
        .find(|(uid, fp)| **uid != kicked.participant_id && *fp == *removed)
    {
        refuse!("P2P removes key {removed}, which surviving member {survivor} claims");
    }
    let head_owners: BTreeSet<String> = exp
        .head
        .dnd
        .as_ref()
        .map(|d| d.owners.iter().cloned().collect())
        .unwrap_or_default();
    // A member reads the head DND after the kick DND became effective, so the
    // kicked owner is already gone from it while its key is still in the P2P.
    // Both shapes therefore mean the same dual-usage party.
    let mut owners_before_kick = head_owners.clone();
    owners_before_kick.insert(kicked.owner_fingerprint.clone());
    let dual_key_party =
        !head_owners.is_empty() && (head_keys == head_owners || head_keys == owners_before_kick);
    if dual_key_party && **removed != kicked.owner_fingerprint {
        refuse!(
            "P2P removes key {removed} but the kicked member's key is {}",
            kicked.owner_fingerprint
        );
    }
    if let Some(claimed) = &kicked.signing_key_fingerprint
        && head_keys.contains(claimed)
        && *removed != claimed
    {
        refuse!("P2P removes key {removed} but the kicked member's recorded key is {claimed}");
    }
    check_p2p_thresholds(p2p, exp)?;
    check_own_host(p2p, exp, false)
}

// ---------------------------------------------------------------------------
// Change threshold
// ---------------------------------------------------------------------------

/// Change-threshold DND: equal to the head except the threshold.
pub fn check_change_threshold_dnd(
    pending: &PendingProposal<DecentralizedNamespaceDefinition>,
    exp: &Expectations,
) -> Check {
    let dnd = &pending.mapping;
    let head = exp.head_dnd()?;
    let proposed: BTreeSet<&String> = dnd.owners.iter().collect();
    let head_owners: BTreeSet<&String> = head.owners.iter().collect();
    if proposed != head_owners || proposed.len() != dnd.owners.len() {
        refuse!(
            "DND owners {:?} differ from the head owners {:?}",
            dnd.owners,
            head.owners
        );
    }
    if dnd.decentralized_namespace != head.decentralized_namespace {
        refuse!(
            "DND namespace {} differs from the head {}",
            dnd.decentralized_namespace,
            head.decentralized_namespace
        );
    }
    check_dnd_threshold(dnd, exp)?;
    check_own_owner(dnd, exp)
}

/// Change-threshold P2P: equal to the head except both thresholds.
pub fn check_change_threshold_p2p(
    pending: &PendingProposal<PartyToParticipant>,
    exp: &Expectations,
) -> Check {
    let p2p = &pending.mapping;
    let head = exp.head_p2p()?;
    if p2p.party != head.party {
        refuse!(
            "P2P party {} differs from the head {}",
            p2p.party,
            head.party
        );
    }
    let hosts = host_tuples(p2p);
    if hosts.len() != p2p.participants.len() {
        refuse!("P2P lists a participant twice");
    }
    if hosts != host_tuples(head) {
        refuse!("P2P hosts {:?} differ from the head hosts", hosts);
    }
    if key_bytes(p2p) != key_bytes(head) {
        refuse!("P2P party signing keys differ from the head keys");
    }
    check_p2p_thresholds(p2p, exp)?;
    check_own_host(p2p, exp, false)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Common checks plus the kind's DND rule, using the DND base serial.
pub fn validate_dnd(
    pending: &PendingProposal<DecentralizedNamespaceDefinition>,
    exp: &Expectations,
    accepted_serial: Option<u32>,
) -> Check {
    check_common(
        pending,
        exp,
        &SerialContext {
            accepted: accepted_serial,
            base: exp.dnd_base_serial,
        },
    )?;
    match exp.kind {
        WorkflowKind::Onboarding => check_onboarding_dnd(pending, exp),
        WorkflowKind::AddParty => check_add_party_dnd(pending, exp),
        WorkflowKind::Kick => check_kick_dnd(pending, exp),
        WorkflowKind::ChangeThreshold => check_change_threshold_dnd(pending, exp),
        other => Err(ValidationError(format!(
            "{other} proposals carry no topology mapping"
        ))),
    }
}

/// Common checks plus the kind's P2P rule, using the P2P base serial.
pub fn validate_p2p(
    pending: &PendingProposal<PartyToParticipant>,
    exp: &Expectations,
    accepted_serial: Option<u32>,
) -> Check {
    check_common(
        pending,
        exp,
        &SerialContext {
            accepted: accepted_serial,
            base: exp.p2p_base_serial,
        },
    )?;
    match exp.kind {
        WorkflowKind::Onboarding => check_onboarding_p2p(pending, exp),
        WorkflowKind::AddParty => check_add_party_p2p(pending, exp),
        WorkflowKind::Kick => check_kick_p2p(pending, exp),
        WorkflowKind::ChangeThreshold => check_change_threshold_p2p(pending, exp),
        other => Err(ValidationError(format!(
            "{other} proposals carry no topology mapping"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
        enums::{ParticipantPermission, TopologyChangeOp},
        party_to_participant::hosting_participant,
    };

    use super::*;
    use crate::onledger::{
        daml::{
            ActiveContract,
            codec::{WorkflowAcceptanceRecord, tests::proposal_full},
        },
        topology::{
            build_add_party_dnd, build_add_party_p2p, build_bootstrap_p2p,
            build_change_threshold_dnd, build_change_threshold_p2p, build_dnd, build_kick_dnd,
            build_kick_p2p, dnd_of, p2p_of,
            tests::{NS, fp, key, participant},
        },
    };

    fn pending<M>(mapping: M, serial: u32, signed_by: &[String]) -> PendingProposal<M> {
        PendingProposal {
            hash_hex: "1220ff".into(),
            serial,
            signed_by: signed_by.to_vec(),
            operation: TopologyChangeOp::AddReplace as i32,
            mapping,
            sequenced: None,
            valid_from: None,
        }
    }

    fn acceptance(participant_n: u8, seed: u8) -> Acceptance {
        ActiveContract {
            contract_id: format!("00acc{participant_n}"),
            offset: 1,
            record: WorkflowAcceptanceRecord {
                proposal: "00proposal".into(),
                proposer: CantonId::parse(&format!("node-a::{NS}")).expect("id"),
                acceptor: CantonId::parse(&format!("node-{participant_n}::{NS}")).expect("id"),
                observers: vec![],
                run_id: "cbtc-creation".into(),
                participant_id: participant(participant_n).to_string(),
                namespace_fingerprint: Some(fp(seed)),
                signing_public_key_hex: Some(hex::encode(key(seed).encode_to_vec())),
                daml_key_fingerprint: Some(fp(seed)),
                member_party: None,
                accepted_at: 1,
            },
        }
    }

    fn identity(participant_n: u8, seed: u8) -> LocalIdentity {
        LocalIdentity {
            participant_id: participant(participant_n),
            owner_fingerprints: [fp(seed)].into_iter().collect(),
            daml_key_fingerprint: Some(fp(seed)),
        }
    }

    fn owner_keys(seeds: &[u8]) -> BTreeMap<String, SigningPublicKey> {
        seeds.iter().map(|s| (fp(*s), key(*s))).collect()
    }

    /// Proposer = participant1 / key 1; acceptors = participant2 / key 2 and
    /// participant3 / key 3; this node = participant2.
    fn onboarding_expectations() -> Expectations {
        let mut record = proposal_full();
        record.kind = WorkflowKind::Onboarding;
        record.proposer_namespace_fingerprint = Some(fp(1));
        record.threshold = Some(2);
        record.dnd_base_serial = None;
        record.p2p_base_serial = None;
        record.new_participant = None;
        record.kicked_participant = None;
        Expectations::new(
            &record,
            &[acceptance(2, 2), acceptance(3, 3)],
            HeadState::default(),
            identity(2, 2),
        )
        .with_owner_keys(owner_keys(&[1, 2, 3]))
        .with_proposer_hosting(true)
    }

    fn onboarding_dnd() -> PendingProposal<DecentralizedNamespaceDefinition> {
        let m = build_dnd(&[fp(1), fp(2), fp(3)], 2);
        pending(dnd_of(&m).expect("dnd").clone(), 1, &[fp(1)])
    }

    fn onboarding_p2p() -> PendingProposal<PartyToParticipant> {
        let namespace = compute_namespace([fp(1), fp(2), fp(3)].iter());
        let m = build_bootstrap_p2p(
            "cbtc",
            &namespace,
            &[participant(1), participant(2), participant(3)],
            &[key(1), key(2), key(3)],
            2,
        );
        pending(p2p_of(&m).expect("p2p").clone(), 1, &[fp(1)])
    }

    fn head() -> HeadState {
        let dnd = dnd_of(&build_dnd(&[fp(1), fp(2), fp(3)], 2))
            .expect("dnd")
            .clone();
        let p2p = p2p_of(&build_bootstrap_p2p(
            "cbtc",
            &dnd.decentralized_namespace,
            &[participant(1), participant(2), participant(3)],
            &[key(1), key(2), key(3)],
            2,
        ))
        .expect("p2p")
        .clone();
        HeadState {
            dnd: Some(dnd),
            p2p: Some(p2p),
        }
    }

    fn party_expectations(kind: WorkflowKind, threshold: i64) -> Expectations {
        let mut record = proposal_full();
        record.kind = kind;
        record.proposer_namespace_fingerprint = Some(fp(1));
        record.threshold = Some(threshold);
        record.previous_threshold = Some(2);
        record.dnd_base_serial = Some(1);
        record.p2p_base_serial = Some(1);
        record.new_participant = Some(participant(4).to_string());
        record.kicked_participant = Some(participant(3).to_string());
        Expectations::new(&record, &[acceptance(4, 4)], head(), identity(2, 2))
            .with_owner_keys(owner_keys(&[4]))
            .with_kicked(KickedMember {
                participant_id: participant(3).to_string(),
                owner_fingerprint: fp(3),
                signing_key_fingerprint: Some(fp(3)),
            })
            .with_survivor_key_claims(
                [
                    (participant(1).to_string(), fp(1)),
                    (participant(2).to_string(), fp(2)),
                ]
                .into_iter()
                .collect(),
            )
    }

    fn head_dnd() -> DecentralizedNamespaceDefinition {
        head().dnd.expect("dnd")
    }

    fn head_p2p() -> PartyToParticipant {
        head().p2p.expect("p2p")
    }

    // ----- common -----

    #[test]
    fn common_accepts_a_first_serial_with_no_base() {
        let exp = onboarding_expectations();
        let ctx = SerialContext {
            accepted: None,
            base: None,
        };
        check_common(&onboarding_dnd(), &exp, &ctx).expect("ok");
    }

    #[test]
    fn common_refuses_a_remove_operation() {
        let exp = onboarding_expectations();
        let mut p = onboarding_dnd();
        p.operation = TopologyChangeOp::Remove as i32;
        let err = check_common(&p, &exp, &SerialContext::default()).expect_err("remove");
        assert!(err.0.contains("REMOVE"), "{err}");
    }

    #[test]
    fn common_refuses_when_the_proposer_has_not_signed() {
        let exp = onboarding_expectations();
        let mut p = onboarding_dnd();
        p.signed_by = vec![fp(9)];
        let err = check_common(&p, &exp, &SerialContext::default()).expect_err("no proposer");
        assert!(err.0.contains("not signed by the proposer"), "{err}");
    }

    #[test]
    fn common_fails_closed_without_a_proposer_fingerprint() {
        let mut exp = onboarding_expectations();
        exp.proposer_fingerprint = None;
        let err =
            check_common(&onboarding_dnd(), &exp, &SerialContext::default()).expect_err("missing");
        assert!(err.0.contains("no proposer namespace fingerprint"), "{err}");
    }

    #[test]
    fn common_refuses_when_own_fingerprint_already_signed() {
        let exp = onboarding_expectations();
        let mut p = onboarding_dnd();
        p.signed_by = vec![fp(1), fp(2)];
        let err = check_common(&p, &exp, &SerialContext::default()).expect_err("already");
        assert!(
            err.0.contains("already carries this node's signature"),
            "{err}"
        );
    }

    #[test]
    fn common_refuses_a_serial_mismatch() {
        let exp = party_expectations(WorkflowKind::ChangeThreshold, 3);
        let m = build_change_threshold_dnd(&head_dnd(), 3);
        let p = pending(dnd_of(&m).expect("dnd").clone(), 3, &[fp(1)]);
        let ctx = SerialContext {
            accepted: Some(1),
            base: Some(1),
        };
        let err = check_common(&p, &exp, &ctx).expect_err("serial");
        assert!(err.0.contains("is not accepted serial 1 + 1"), "{err}");
    }

    #[test]
    fn common_refuses_a_base_serial_mismatch() {
        let exp = party_expectations(WorkflowKind::ChangeThreshold, 3);
        let m = build_change_threshold_dnd(&head_dnd(), 3);
        let p = pending(dnd_of(&m).expect("dnd").clone(), 3, &[fp(1)]);
        let ctx = SerialContext {
            accepted: Some(2),
            base: Some(1),
        };
        let err = check_common(&p, &exp, &ctx).expect_err("base");
        assert!(err.0.contains("topology moved"), "{err}");
    }

    #[test]
    fn common_refuses_a_first_serial_other_than_one() {
        let exp = onboarding_expectations();
        let mut p = onboarding_dnd();
        p.serial = 2;
        let err = check_common(&p, &exp, &SerialContext::default()).expect_err("serial 2");
        assert!(err.0.contains("serial 1"), "{err}");
    }

    // ----- onboarding -----

    #[test]
    fn onboarding_dnd_and_p2p_pass() {
        let exp = onboarding_expectations();
        validate_dnd(&onboarding_dnd(), &exp, None).expect("dnd ok");
        validate_p2p(&onboarding_p2p(), &exp, None).expect("p2p ok");
    }

    #[test]
    fn onboarding_dnd_refuses_a_foreign_owner() {
        let exp = onboarding_expectations();
        let m = build_dnd(&[fp(1), fp(2), fp(9)], 2);
        let p = pending(dnd_of(&m).expect("dnd").clone(), 1, &[fp(1)]);
        let err = check_onboarding_dnd(&p, &exp).expect_err("foreign");
        assert!(err.0.contains("differ from the accepted set"), "{err}");
    }

    #[test]
    fn onboarding_dnd_refuses_a_wrong_threshold() {
        let exp = onboarding_expectations();
        let m = build_dnd(&[fp(1), fp(2), fp(3)], 3);
        let p = pending(dnd_of(&m).expect("dnd").clone(), 1, &[fp(1)]);
        let err = check_onboarding_dnd(&p, &exp).expect_err("threshold");
        assert!(err.0.contains("differs from the accepted 2"), "{err}");
    }

    #[test]
    fn onboarding_dnd_refuses_a_namespace_not_derived_from_owners() {
        let exp = onboarding_expectations();
        let mut p = onboarding_dnd();
        p.mapping.decentralized_namespace = NS.into();
        let err = check_onboarding_dnd(&p, &exp).expect_err("namespace");
        assert!(err.0.contains("not derived from its owners"), "{err}");
    }

    #[test]
    fn onboarding_p2p_refuses_a_host_permission_flip() {
        let exp = onboarding_expectations();
        let mut p = onboarding_p2p();
        p.mapping.participants[0].permission = ParticipantPermission::Submission as i32;
        let err = check_onboarding_p2p(&p, &exp).expect_err("permission");
        assert!(err.0.contains("hosts"), "{err}");
    }

    #[test]
    fn onboarding_p2p_refuses_an_onboarding_marker() {
        let exp = onboarding_expectations();
        let mut p = onboarding_p2p();
        p.mapping.participants[1].onboarding = Some(hosting_participant::Onboarding {});
        let err = check_onboarding_p2p(&p, &exp).expect_err("marker");
        assert!(err.0.contains("hosts"), "{err}");
    }

    #[test]
    fn onboarding_p2p_refuses_a_key_not_from_the_root_delegations() {
        let exp = onboarding_expectations();
        let mut p = onboarding_p2p();
        p.mapping.party_signing_keys.as_mut().expect("keys").keys[0] = key(9);
        let err = check_onboarding_p2p(&p, &exp).expect_err("keys");
        assert!(err.0.contains("root delegation keys"), "{err}");
    }

    #[test]
    fn onboarding_p2p_refuses_a_signing_threshold_drift() {
        let exp = onboarding_expectations();
        let mut p = onboarding_p2p();
        p.mapping
            .party_signing_keys
            .as_mut()
            .expect("keys")
            .threshold = 1;
        let err = check_onboarding_p2p(&p, &exp).expect_err("signing threshold");
        assert!(err.0.contains("signing threshold 1"), "{err}");
    }

    #[test]
    fn onboarding_p2p_refuses_an_unverified_proposer_host() {
        let exp = onboarding_expectations().with_proposer_hosting(false);
        let err = check_onboarding_p2p(&onboarding_p2p(), &exp).expect_err("hosting");
        assert!(err.0.contains("Submission"), "{err}");
    }

    #[test]
    fn onboarding_p2p_refuses_a_missing_own_host() {
        let mut exp = onboarding_expectations();
        exp.identity.participant_id = participant(9);
        let err = check_onboarding_p2p(&onboarding_p2p(), &exp).expect_err("own host");
        assert!(err.0.contains("does not host this node"), "{err}");
    }

    // ----- add party -----

    #[test]
    fn add_party_dnd_and_p2p_pass_for_a_member_and_for_the_joiner() {
        let exp = party_expectations(WorkflowKind::AddParty, 3);
        let dnd = build_add_party_dnd(&head_dnd(), &fp(4), 3);
        let dnd = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        validate_dnd(&dnd, &exp, Some(1)).expect("dnd ok");
        let p2p = build_add_party_p2p(&head_p2p(), &participant(4), &key(4), 3);
        let p2p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        validate_p2p(&p2p, &exp, Some(1)).expect("p2p ok");

        let mut joiner_exp = exp.clone();
        joiner_exp.identity = identity(4, 4);
        check_add_party_p2p(&p2p, &joiner_exp).expect("joiner ok");
    }

    #[test]
    fn add_party_p2p_refuses_a_marker_on_a_survivor() {
        let exp = party_expectations(WorkflowKind::AddParty, 3);
        let p2p = build_add_party_p2p(&head_p2p(), &participant(4), &key(4), 3);
        let mut p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        p.mapping.participants[0].onboarding = Some(hosting_participant::Onboarding {});
        let err = check_add_party_p2p(&p, &exp).expect_err("marker");
        assert!(
            err.0.contains("head hosts plus the onboarding joiner"),
            "{err}"
        );
    }

    #[test]
    fn add_party_dnd_refuses_a_foreign_owner() {
        let exp = party_expectations(WorkflowKind::AddParty, 3);
        let dnd = build_add_party_dnd(&head_dnd(), &fp(9), 3);
        let p = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        let err = check_add_party_dnd(&p, &exp).expect_err("foreign");
        assert!(err.0.contains("head owners plus the joiner"), "{err}");
    }

    #[test]
    fn add_party_p2p_refuses_a_permission_flip_on_a_survivor() {
        let exp = party_expectations(WorkflowKind::AddParty, 3);
        let p2p = build_add_party_p2p(&head_p2p(), &participant(4), &key(4), 3);
        let mut p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        p.mapping.participants[1].permission = ParticipantPermission::Submission as i32;
        let err = check_add_party_p2p(&p, &exp).expect_err("permission");
        assert!(
            err.0.contains("head hosts plus the onboarding joiner"),
            "{err}"
        );
    }

    // ----- kick -----

    #[test]
    fn kick_dnd_and_p2p_pass() {
        let exp = party_expectations(WorkflowKind::Kick, 2);
        let dnd = build_kick_dnd(&head_dnd(), &fp(3), 2);
        let dnd = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        validate_dnd(&dnd, &exp, Some(1)).expect("dnd ok");
        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(3), 2);
        let p2p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        validate_p2p(&p2p, &exp, Some(1)).expect("p2p ok");
    }

    #[test]
    fn kick_dnd_uses_the_local_cache_not_the_proposer() {
        let mut exp = party_expectations(WorkflowKind::Kick, 2);
        exp.kicked = None;
        let dnd = build_kick_dnd(&head_dnd(), &fp(3), 2);
        let p = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        let err = check_kick_dnd(&p, &exp).expect_err("no cache");
        assert!(err.0.contains("no cached owner key"), "{err}");
    }

    #[test]
    fn kick_dnd_refuses_removing_the_wrong_owner() {
        let exp = party_expectations(WorkflowKind::Kick, 2);
        let dnd = build_kick_dnd(&head_dnd(), &fp(2), 2);
        let p = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        let err = check_kick_dnd(&p, &exp).expect_err("wrong owner");
        assert!(err.0.contains("head owners minus the kicked"), "{err}");
    }

    #[test]
    fn kick_p2p_refuses_removing_own_key() {
        let exp = party_expectations(WorkflowKind::Kick, 2);
        // Host removal is right, key removal targets this node (key 2).
        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(2), 2);
        let p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        let err = check_kick_p2p(&p, &exp).expect_err("own key");
        assert!(err.0.contains("this node's own Daml signing key"), "{err}");
    }

    #[test]
    fn kick_p2p_refuses_removing_a_survivors_claimed_key() {
        let mut exp = party_expectations(WorkflowKind::Kick, 2);
        exp.identity.daml_key_fingerprint = None;
        // Legacy party: signing keys are not the DND owners.
        exp.head.dnd = None;
        exp.kicked = Some(KickedMember {
            participant_id: participant(3).to_string(),
            owner_fingerprint: fp(30),
            signing_key_fingerprint: None,
        });
        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(1), 2);
        let p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        let err = check_kick_p2p(&p, &exp).expect_err("claimed");
        assert!(err.0.contains("surviving member"), "{err}");
    }

    #[test]
    fn kick_p2p_legacy_branch_accepts_the_one_unclaimed_key() {
        let mut exp = party_expectations(WorkflowKind::Kick, 2);
        exp.head.dnd = None;
        exp.kicked = Some(KickedMember {
            participant_id: participant(3).to_string(),
            owner_fingerprint: fp(30),
            signing_key_fingerprint: None,
        });
        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(3), 2);
        let p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        check_kick_p2p(&p, &exp).expect("legacy ok");
    }

    /// A member validates the kick P2P after the kick DND became effective, so
    /// the head owners already exclude the kicked member. The party is still
    /// dual-usage, and the pin on the kicked member's key must still hold.
    #[test]
    fn kick_p2p_pins_the_kicked_key_after_the_dnd_landed() {
        let mut exp = party_expectations(WorkflowKind::Kick, 2);
        // The head DND is the one the kick already wrote.
        let mut landed = head_dnd();
        landed.owners.retain(|o| *o != fp(3));
        landed.threshold = 2;
        exp.head.dnd = Some(landed);
        // A cold cache: no own Daml key, no survivor claims, no recorded key for
        // the kicked member. Only the owner-set shape can catch a wrong removal.
        exp.identity.daml_key_fingerprint = None;
        exp.survivor_key_claims = Default::default();
        exp.kicked = Some(KickedMember {
            participant_id: participant(3).to_string(),
            owner_fingerprint: fp(3),
            signing_key_fingerprint: None,
        });

        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(1), 2);
        let p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        let err = check_kick_p2p(&p, &exp).expect_err("wrong key");
        assert!(err.0.contains("the kicked member's key is"), "{err}");

        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(3), 2);
        let p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        check_kick_p2p(&p, &exp).expect("the kicked member's own key");
    }

    #[test]
    fn kick_p2p_refuses_removing_two_keys_or_none() {
        let exp = party_expectations(WorkflowKind::Kick, 2);
        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(3), 2);
        let mut p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        p.mapping
            .party_signing_keys
            .as_mut()
            .expect("keys")
            .keys
            .pop();
        let err = check_kick_p2p(&p, &exp).expect_err("two");
        assert!(err.0.contains("removes exactly one"), "{err}");

        let mut none = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        none.mapping = PartyToParticipant {
            participants: none.mapping.participants.clone(),
            party_signing_keys: head_p2p().party_signing_keys.clone().map(|mut k| {
                k.threshold = 2;
                k
            }),
            ..none.mapping
        };
        let err = check_kick_p2p(&none, &exp).expect_err("none");
        assert!(err.0.contains("removes 0"), "{err}");
    }

    #[test]
    fn kick_p2p_refuses_a_permission_flip_on_a_survivor() {
        let exp = party_expectations(WorkflowKind::Kick, 2);
        let p2p = build_kick_p2p(&head_p2p(), &participant(3), &fp(3), 2);
        let mut p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        p.mapping.participants[0].permission = ParticipantPermission::Observation as i32;
        let err = check_kick_p2p(&p, &exp).expect_err("permission");
        assert!(err.0.contains("head hosts minus the kicked"), "{err}");
    }

    // ----- change threshold -----

    #[test]
    fn change_threshold_dnd_and_p2p_pass() {
        let exp = party_expectations(WorkflowKind::ChangeThreshold, 3);
        let dnd = build_change_threshold_dnd(&head_dnd(), 3);
        let dnd = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        validate_dnd(&dnd, &exp, Some(1)).expect("dnd ok");
        let p2p = build_change_threshold_p2p(&head_p2p(), 3);
        let p2p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        validate_p2p(&p2p, &exp, Some(1)).expect("p2p ok");
    }

    #[test]
    fn change_threshold_refuses_a_wrong_threshold() {
        let exp = party_expectations(WorkflowKind::ChangeThreshold, 3);
        let dnd = build_change_threshold_dnd(&head_dnd(), 1);
        let p = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        let err = check_change_threshold_dnd(&p, &exp).expect_err("threshold");
        assert!(err.0.contains("differs from the accepted 3"), "{err}");
    }

    #[test]
    fn change_threshold_refuses_a_membership_change() {
        let exp = party_expectations(WorkflowKind::ChangeThreshold, 3);
        let dnd = build_add_party_dnd(&head_dnd(), &fp(9), 3);
        let p = pending(dnd_of(&dnd).expect("dnd").clone(), 2, &[fp(1)]);
        let err = check_change_threshold_dnd(&p, &exp).expect_err("owners");
        assert!(err.0.contains("differ from the head owners"), "{err}");

        let p2p = build_change_threshold_p2p(&head_p2p(), 3);
        let mut p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        p.mapping.participants[2].onboarding = Some(hosting_participant::Onboarding {});
        let err = check_change_threshold_p2p(&p, &exp).expect_err("hosts");
        assert!(err.0.contains("differ from the head hosts"), "{err}");
    }

    #[test]
    fn change_threshold_refuses_a_signing_threshold_left_behind() {
        let exp = party_expectations(WorkflowKind::ChangeThreshold, 3);
        let p2p = build_change_threshold_p2p(&head_p2p(), 3);
        let mut p = pending(p2p_of(&p2p).expect("p2p").clone(), 2, &[fp(1)]);
        p.mapping
            .party_signing_keys
            .as_mut()
            .expect("keys")
            .threshold = 2;
        let err = check_change_threshold_p2p(&p, &exp).expect_err("signing");
        assert!(err.0.contains("signing threshold 2"), "{err}");
    }

    #[test]
    fn contracts_and_dars_carry_no_topology_mapping() {
        let mut exp = onboarding_expectations();
        exp.kind = WorkflowKind::Dars;
        let err = validate_dnd(&onboarding_dnd(), &exp, None).expect_err("no mapping");
        assert!(err.0.contains("no topology mapping"), "{err}");
    }

    #[test]
    fn tuples_and_key_sets_compare_the_full_shape() {
        let p2p = head_p2p();
        let tuples = host_tuples(&p2p);
        assert_eq!(tuples.len(), 3);
        assert!(
            tuples.iter().all(
                |t| !t.onboarding && t.permission == ParticipantPermission::Confirmation as i32
            )
        );
        assert_eq!(key_bytes(&p2p).len(), 3);
        assert_eq!(
            key_fingerprints(&p2p),
            [fp(1), fp(2), fp(3)].into_iter().collect()
        );
    }
}
