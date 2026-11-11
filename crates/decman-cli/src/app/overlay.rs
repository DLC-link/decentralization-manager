//! The overlay enum and the state of every form and popup.

use anyhow::Result;
use serde_json::Value;

use common::types::{AuditLogEntry, PeerPackageComparison};

use crate::api::{ChainAuditEntry, DomainGovAction, FeedItem, GovAction, GovState, Holding};
use crate::api::{KeyStatus, NetworkInfo, PartyAuthStatus, PeerEntry};
use crate::composer::{Composer, TypeOption};

/// Per-party detail extras (holdings + audit), each fetched independently so
/// one failing does not hide the other.
pub struct DetailData {
    pub holdings: Result<Vec<Holding>, String>,
    pub audit: Result<Vec<AuditLogEntry>, String>,
    pub gov_state: Result<Option<GovState>, String>,
}

/// This node's key status and DSO network identity, each fetched independently
/// so one failing (e.g. the DSO API is unreachable) does not hide the other.
pub struct NodeInfoData {
    pub key: Result<KeyStatus, String>,
    pub network: Result<NetworkInfo, String>,
}

/// A DAR file picked from disk for upload / distribution.
pub struct Dar {
    pub filename: String,
    pub data: String,
}

/// A peer toggled in the distribution dialog.
pub struct PeerChoice {
    pub id: String,
    pub name: String,
    pub checked: bool,
}

/// The onboarding (create-party) form: a party-id prefix plus a tickable list
/// of peers to invite. `cursor` 0 selects the prefix field; `1..=peers.len()`
/// select a peer row.
pub struct OnboardForm {
    pub prefix: String,
    pub peers: Vec<PeerChoice>,
    pub cursor: usize,
}

impl OnboardForm {
    /// Whether the prefix field (cursor 0) is focused.
    pub(super) fn on_prefix(&self) -> bool {
        self.cursor == 0
    }

    /// The peer row index under the cursor, if a peer row is focused.
    pub(super) fn peer_index(&self) -> Option<usize> {
        self.cursor.checked_sub(1)
    }
}

/// A participant that may be kicked from a party (an owner of the party).
pub struct KickCandidate {
    pub participant_id: String,
    pub label: String,
}

/// The kick-participant form: pick an owner to remove and set the new
/// signing threshold for the remaining owners.
pub struct KickForm {
    pub party_id: String,
    pub party_name: String,
    pub previous_threshold: i32,
    pub candidates: Vec<KickCandidate>,
    pub selected: usize,
    pub new_threshold: i32,
    /// Highest threshold allowed after the kick (remaining owner count).
    pub max_threshold: i32,
}

/// The add-party form: pick a configured peer that is not yet a member, and
/// set the signing threshold for the party once it has joined.
pub struct AddPartyForm {
    pub party_id: String,
    pub party_name: String,
    pub previous_threshold: i32,
    /// Configured peers not already in the party (self excluded).
    pub candidates: Vec<KickCandidate>,
    pub selected: usize,
    pub new_threshold: i32,
    /// Highest threshold allowed after the add (current members + the new one).
    pub max_threshold: i32,
}

/// The change-threshold form: adjust the signing threshold of an existing
/// party, keeping its membership unchanged.
pub struct ChangeThresholdForm {
    pub party_id: String,
    pub party_name: String,
    pub previous_threshold: i32,
    pub new_threshold: i32,
    /// Highest threshold allowed (the party's member count).
    pub max_threshold: i32,
}

/// One pending governance item shown in the approvals overlay: an off-chain
/// action or an on-chain (core-domain) proposal.
pub enum GovItem {
    OffChain(GovAction),
    Domain(DomainGovAction),
}

/// State of the governance-approvals overlay for one party.
pub struct GovView {
    pub party_name: String,
    pub party_id: String,
    pub rules_contract_id: String,
    pub member_party_id: String,
    pub threshold: i32,
    pub items: Vec<GovItem>,
    pub selected: usize,
}

/// A governance mutation to perform against a pending action.
pub(super) enum GovOp {
    Confirm {
        action: Value,
        governance_type: String,
        proposal_cid: Option<String>,
    },
    Execute {
        action: Value,
        confirmation_cids: Vec<String>,
        governance_type: String,
        proposal_cid: Option<String>,
    },
    Cancel {
        confirmation_cid: String,
        governance_type: String,
    },
    Expire {
        confirmation_cid: String,
        governance_type: String,
    },
    /// Propose a new on-chain action (`POST /governance/propose`).
    Propose { proposal: Value },
}

/// Which composer the type picker is choosing for.
#[derive(Clone, Copy)]
pub enum ComposerKind {
    /// A brand-new off-chain governance action (submitted via confirm).
    Action,
    /// A new on-chain governance proposal.
    Proposal,
}

/// The action / proposal type picker, before a specific form is opened.
pub struct ComposerPick {
    pub kind: ComposerKind,
    pub party_id: String,
    pub party_name: String,
    pub rules_contract_id: String,
    pub default_threshold: i64,
    pub options: Vec<TypeOption>,
    pub selected: usize,
}

/// The governance-core contract deployment form. The member set, participant
/// ids and package id are resolved from the server; the operator only edits the
/// initial governance threshold and the proposal timeout (the contract's field
/// structure is fixed, mirroring the web frontend's locked gov-core preset).
pub struct DeployForm {
    pub party_id: String,
    pub party_name: String,
    /// The `governance_core` package id (empty if the DAR is not vetted).
    pub package_id: String,
    pub operator_party: String,
    /// Participant uids, aligned with `member_parties`.
    pub participant_ids: Vec<String>,
    /// Member party ids (the governance member set).
    pub member_parties: Vec<String>,
    pub threshold: String,
    pub timeout_micros: String,
    /// 0 = threshold, 1 = timeout, 2 = submit row.
    pub cursor: usize,
}

/// A governance mutation plus the context needed to refresh the overlay after.
pub struct GovActionRequest {
    pub(super) op: GovOp,
    pub(super) party_id: String,
    pub(super) party_name: String,
    pub(super) rules_contract_id: String,
}

/// The add-peer sub-form of the network-config editor.
pub struct PeerForm {
    pub participant_id: String,
    pub name: String,
    pub address: String,
    pub port: String,
    pub public_key: String,
    pub party: String,
    /// 0..=5 select a field; 6 is the submit row.
    pub cursor: usize,
}

impl PeerForm {
    pub(super) fn blank() -> Self {
        Self {
            participant_id: String::new(),
            name: String::new(),
            address: String::new(),
            port: String::new(),
            public_key: String::new(),
            party: String::new(),
            cursor: 0,
        }
    }
}

/// State of the network-config editor: the editable peer list, plus an
/// optional in-progress add-peer form.
pub struct NetworkEditState {
    pub peers: Vec<PeerEntry>,
    pub selected: usize,
    pub adding: Option<PeerForm>,
}

/// Which IdP the party-config form is editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IdpMode {
    Keycloak,
    Auth0,
}

/// One row in the party-config form (a field, the provider toggle, or an action).
#[derive(Clone, Copy)]
pub enum PcRow {
    Mode,
    MemberParty,
    UserId,
    KcUrl,
    KcRealm,
    KcClientId,
    KcSecret,
    Auth0Domain,
    Auth0Audience,
    Auth0ClientId,
    Auth0Secret,
    Discover,
    Submit,
}

/// The per-party IdP configuration editor (Keycloak or Auth0).
pub struct PartyConfigForm {
    pub dec_party_id: String,
    pub party_name: String,
    pub mode: IdpMode,
    pub member_party_id: String,
    pub user_id: String,
    pub kc_url: String,
    pub kc_realm: String,
    pub kc_client_id: String,
    /// Empty leaves the stored secret unchanged when one is set.
    pub kc_secret: String,
    pub kc_has_secret: bool,
    pub auth0_domain: String,
    pub auth0_audience: String,
    pub auth0_client_id: String,
    pub auth0_secret: String,
    pub auth0_has_secret: bool,
    pub cursor: usize,
    /// Transient status line (discover result / error), shown in the form.
    pub status: String,
}

impl PartyConfigForm {
    /// The rows shown for the current provider mode, in display order.
    pub fn rows(&self) -> Vec<PcRow> {
        let mut rows = vec![PcRow::Mode, PcRow::MemberParty, PcRow::UserId];
        match self.mode {
            IdpMode::Keycloak => {
                rows.extend([
                    PcRow::KcUrl,
                    PcRow::KcRealm,
                    PcRow::KcClientId,
                    PcRow::KcSecret,
                ]);
            }
            IdpMode::Auth0 => rows.extend([
                PcRow::Auth0Domain,
                PcRow::Auth0Audience,
                PcRow::Auth0ClientId,
                PcRow::Auth0Secret,
            ]),
        }
        rows.extend([PcRow::Discover, PcRow::Submit]);
        rows
    }
}

/// The grant-rights form: an admin client id + secret used to grant act/read
/// rights for one party. The secret is sent once and not stored.
pub struct GrantForm {
    pub dec_party_id: String,
    pub party_name: String,
    pub client_id: String,
    pub client_secret: String,
    /// 0 = client id, 1 = secret, 2 = submit row.
    pub cursor: usize,
}

/// A modal overlay drawn above the main UI.
pub enum Overlay {
    None,
    /// A blocking action is in flight.
    Busy(String),
    /// A result or error message awaiting dismissal.
    Message(String),
    /// The package checker results.
    Compare {
        comparison: PeerPackageComparison,
        scroll: u16,
    },
    /// A tickable peer list for DAR distribution.
    PeerSelect {
        dar: Dar,
        peers: Vec<PeerChoice>,
        cursor: usize,
    },
    /// Syntax-highlighted JSON for an expanded audit entry.
    Json {
        value: Value,
        scroll: u16,
    },
    /// The onboarding (create-party) form.
    Onboard(OnboardForm),
    /// The kick-participant form.
    Kick(KickForm),
    /// The add-party form (add a new member to an existing party).
    AddParty(AddPartyForm),
    /// The change-threshold form (change a party's signing threshold).
    ChangeThreshold(ChangeThresholdForm),
    /// A scrollable detail view of a feed item (workflow run or invitation).
    /// Boxed: a feed item (with its workflow run) is large relative to the
    /// other overlay variants.
    FeedDetail {
        item: Box<FeedItem>,
        scroll: u16,
    },
    /// The on-chain governance audit trail for the open party.
    ChainAudit {
        entries: Vec<ChainAuditEntry>,
        scroll: u16,
    },
    /// Per-party authentication status and rights.
    Auth {
        parties: Vec<PartyAuthStatus>,
        selected: usize,
    },
    /// The grant-rights form for a selected party.
    GrantRights(Box<GrantForm>),
    /// The governance approvals list for a party (confirm / execute / revoke).
    Governance(Box<GovView>),
    /// The action / proposal type picker.
    ComposerPick(Box<ComposerPick>),
    /// A composer form for a chosen action / proposal variant.
    Composer(Box<Composer>),
    /// The governance-core contract deployment form.
    Deploy(Box<DeployForm>),
    /// The network-config (peer list) editor.
    NetworkEdit(Box<NetworkEditState>),
    /// The per-party IdP configuration editor.
    PartyConfig(Box<PartyConfigForm>),
    /// This node's key status and DSO network identity.
    NodeInfo(Box<NodeInfoData>),
}
