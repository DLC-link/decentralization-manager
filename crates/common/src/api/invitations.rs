//! Node key status and the peer-to-peer invitation payloads.

use serde::{Deserialize, Serialize};

use crate::{
    canton_id::CantonId,
    types::{HandledInvitation, PendingInvitation, WorkflowKind},
};

/// Response for key status check
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KeyStatusResponse {
    pub has_keys: bool,
    pub public_key: Option<String>,
}

/// Payload sent inside an `InviteOnboarding` Noise message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OnboardingInvitePayload {
    pub prefix: String,
    pub participants: Vec<CantonId>,
    /// Initial signing threshold the coordinator will use, so the invitation
    /// and run cards can show it before the proposals are built. `None` from
    /// an older coordinator that predates the field (the card then omits it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<i32>,
    /// The confirmation threshold the coordinator will propose. `None` means
    /// it equals `threshold`, which is all an older coordinator sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_threshold: Option<i32>,
    /// The coordinator's `workflow_runs` instance name for this run. Echoed
    /// back in `DeclineInvitationPayload` so the coordinator can tell a
    /// decline of THIS run apart from a stale invite of an earlier run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside a `DeclineInvitation` Noise message — peer telling
/// the coordinator that it has rejected an outstanding invitation so the
/// coordinator can fail its matching in-progress run with a clear error.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DeclineInvitationPayload {
    pub kind: WorkflowKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The coordinator run this decline targets (from the invite payload).
    /// `None` when the invite predates this field — the coordinator then
    /// falls back to kind + membership checks only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
    /// Set when the peer accepted the invitation and later gave up on the
    /// run, with `reason` saying why. The coordinator then counts the peer
    /// out of the run instead of treating it as a decline. A coordinator
    /// that predates the field ignores it and fails the run as declined.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abandoned: bool,
}

/// Payload sent inside an `InviteDars` Noise message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DarsInvitePayload {
    pub dar_filenames: Vec<String>,
    /// SHA-256 of each DAR's content, hex encoded and index-aligned with
    /// `dar_filenames`. A peer pins the accepted content with these, so a
    /// coordinator cannot get a different DAR vetted under an accepted name.
    /// Empty from a coordinator that predates the field; the peer then falls
    /// back to checking filenames only.
    #[serde(default)]
    pub dar_hashes: Vec<String>,
    /// The member set (selected peers) this distribution targets, so the peer
    /// card can render the same participant list the coordinator shows.
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Peer -> coordinator: "send me this DAR of that run, I want to read it
/// before I accept". The index is into the invite's `dar_filenames`, so a peer
/// can only name a DAR it was offered, and the filename rides along for the
/// coordinator to check that the two still agree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestDarPayload {
    /// The coordinator's run instance name, from the invite.
    pub workflow_instance: String,
    /// Index into the invite's `dar_filenames`.
    pub index: usize,
    /// The filename at that index, as the invite gave it.
    pub filename: String,
}

/// Payload sent inside an `InviteKick` Noise message — gives the peer enough
/// context to show "kicking X from dec party Y, threshold a→b" before the kick
/// proposals arrive later in the workflow.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KickInvitePayload {
    pub dec_party_id: CantonId,
    /// The participant that leaves. `None` for an owner key that no peer has
    /// claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicked_participant: Option<CantonId>,
    /// The namespace owner key the kick removes. Absent from coordinators
    /// that predate the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicked_owner_key: Option<String>,
    pub new_threshold: i32,
    pub previous_threshold: i32,
    /// The confirmation threshold the coordinator will propose. `None` means
    /// it equals `new_threshold`, which is all an older coordinator sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_threshold: Option<i32>,
    /// The surviving member set the kick targets, so the peer card renders
    /// the same participant list the coordinator shows.
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside an `InviteAddParty` Noise message — gives the peer
/// enough context to show "adding X to dec party Y, threshold a→b" before
/// the proposals arrive. Sent both to the existing members and to the new
/// member itself (which recognises its own id in `new_participant`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AddPartyInvitePayload {
    pub dec_party_id: CantonId,
    pub new_participant: CantonId,
    pub new_threshold: i32,
    pub previous_threshold: i32,
    /// The confirmation threshold the coordinator will propose. `None` means
    /// it equals `new_threshold`, which is all an older coordinator sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_threshold: Option<i32>,
    /// The full post-add member set, so the peer card renders the same
    /// participant list the coordinator shows.
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside an `InviteChangeThreshold` Noise message — gives the
/// peer enough context to show "changing dec party Y threshold a→b" before the
/// proposals arrive.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ChangeThresholdInvitePayload {
    pub dec_party_id: CantonId,
    pub new_threshold: i32,
    pub previous_threshold: i32,
    /// The confirmation threshold the coordinator will propose. `None` means
    /// it equals `new_threshold`, which is all an older coordinator sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_threshold: Option<i32>,
    /// The party's member set, so the peer card renders the same participant
    /// list the coordinator shows.
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside an `InviteContracts` Noise message — mirrors the rich
/// Kick payload so the peer card can show the dec party, member set, and the
/// package/contract names being deployed before the proposals arrive.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractsInvitePayload {
    pub dec_party_id: CantonId,
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// Human-readable contract/package names (from `ContractDefinition.name`).
    #[serde(default)]
    pub package_names: Vec<String>,
    /// Ordered creates committed to before acceptance, independent of ledger preparation.
    #[serde(default)]
    pub contract_intents: Vec<ContractDeploymentIntent>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// The exact create a contracts invitation authorizes. The argument hash uses
/// DecMan's normalized Daml value encoding (record labels and type annotations omitted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractDeploymentIntent {
    pub package_id: String,
    pub module_name: String,
    pub entity_name: String,
    pub argument_hash: String,
}

/// Response for pending invitations endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PendingInvitationsResponse {
    pub invitations: Vec<PendingInvitation>,
}

/// Response for the handled invitations endpoint, newest first
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct HandledInvitationsResponse {
    pub invitations: Vec<HandledInvitation>,
}

/// Request to accept or decline an invitation
#[derive(Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InvitationActionRequest {
    pub id: String,
}
