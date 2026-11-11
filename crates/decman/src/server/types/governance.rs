//! Governance requests and responses, reward status and audit queries.

use decman_lib::catalog::action::ActionType;

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;
use common::api::PAGE_SIZE;
use common::api::{
    ChainAuditEntry, CouponReassignmentDelegationSummary, DisclosedContractInput, GovernanceType,
};
use decman_lib::catalog::types::{
    AcceptTransferDetails, RewardBeneficiary, ServiceRequestDetails, TransferProposalDetails,
};

use super::proposal::ProposalType;

/// Request to propose a governance domain action (creates proposal contract)
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProposeActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub proposal: ProposalType,
}

/// A pending domain action proposal with its confirmations
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DomainGovernanceAction {
    /// Contract ID of the proposal
    pub proposal_cid: String,
    /// Human-readable label (e.g., "SetupCcPreapproval")
    pub action_label: String,
    /// Human-readable description from the proposal's GovernableActionView
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Confirmations for this proposal
    pub confirmations: Vec<DomainConfirmation>,
    /// Number of unique confirmers
    pub confirmation_count: usize,
    /// Contract ids of the confirmations Execute submits
    pub executable_confirmation_cids: Vec<String>,
    /// Whether threshold is met for execution
    pub can_execute: bool,
    /// `true` when the underlying proposal contract was not found in this
    /// participant's ACS at query time. Confirmations referencing an archived
    /// proposal can't be confirmed/executed (the proposal cid is gone), but
    /// the Confirmation contracts themselves are still active and need to be
    /// expired explicitly to clear them off the ledger. The UI uses this
    /// flag to render a dismiss-only card instead of the normal Confirm /
    /// Execute affordances.
    #[serde(default)]
    pub orphaned: bool,
    /// Structured Transfer-proposal fields (recipient, amount, instrument)
    /// pulled from the on-chain `TransferProposal` contract so the
    /// notification card can display what's actually being transferred
    /// without the user having to inspect the contract CID. Only populated
    /// for `Transfer` proposals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_details: Option<TransferProposalDetails>,
    /// Sender / amount / instrument resolved from the `TransferInstruction`
    /// referenced by an `AcceptTransferProposal`. Lets the notification card
    /// show the operator what they're approving (who sent what) without a
    /// follow-up fetch from the UI. Only populated for `AcceptTransfer`
    /// proposals, and only when the linked instruction was readable at query
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_transfer_details: Option<AcceptTransferDetails>,
    /// Operator plus the counterparty (user or provider) pulled from a
    /// `CreateUserServiceRequest` / `CreateProviderServiceRequest` proposal so
    /// the notification card shows the full summary — proposal type (the
    /// `action_label`), operator party, and the user or provider party — without
    /// the operator having to inspect the contract. Only populated for those two
    /// proposal kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_request_details: Option<ServiceRequestDetails>,
    /// The member who created the proposal, read from the proposal contract.
    /// Only that member can retract it with `GovernableAction_ProposerCancel`,
    /// so the card shows the retract button when this equals the node's own
    /// member party. Absent on an orphaned card, where the proposal contract
    /// is no longer readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer: Option<CantonId>,
    /// `true` when the live rules contract lists `proposer` neither as a
    /// member nor as an additional proposer. `GovernanceRules_ConfirmAction`
    /// then rejects every confirmation, so the card withholds Confirm. `false`
    /// when the rules contract or the proposer could not be read.
    #[serde(default)]
    pub proposer_not_authorized: bool,
    /// Ledger effective time of the proposal's create event, in seconds. The
    /// notification feed sorts on this, so a proposal holds its place between
    /// refreshes whether or not anyone has confirmed it. Absent on an orphaned
    /// card, where the proposal contract is no longer readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// Template of the proposal contract, as `package-id:Module:Entity`.
    /// Absent on an orphaned card, where the proposal contract is no longer
    /// readable. Withheld like `proposal_payload`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_template_id: Option<String>,
    /// Every create argument of the proposal contract, so a member can read
    /// all of what they confirm or execute rather than only the summary
    /// fields. Same JSON shape as the chain audit's `details`: record fields
    /// keep their labels, numerics and parties are strings, a variant is
    /// `{"_variant": ..., "value": ...}`. Absent on an orphaned card, where
    /// the proposal contract is no longer readable. When `DECPM_ADMIN_ROLE`
    /// names a role, a caller that lacks that role gets neither this field
    /// nor `proposal_template_id`. When it is unset, every authenticated
    /// caller passes the admin check and gets both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typegen", ts(type = "unknown"))]
    pub proposal_payload: Option<serde_json::Value>,
}

/// Request to submit a confirmation for an action with structured type
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ConfirmActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
    #[serde(default)]
    pub governance_type: GovernanceType,
    /// For CoreDomain: ContractId of the GovernableAction proposal
    #[serde(default)]
    pub proposal_cid: Option<String>,
}

/// Request to execute a confirmed action with structured type
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExecuteActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
    pub confirmation_cids: Vec<String>,
    #[serde(default)]
    pub disclosed_contracts: Vec<DisclosedContractInput>,
    #[serde(default)]
    pub governance_type: GovernanceType,
    /// For CoreDomain: ContractId of the GovernableAction proposal
    #[serde(default)]
    pub proposal_cid: Option<String>,
}

/// A single governance confirmation with parsed action
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceConfirmation {
    pub contract_id: String,
    pub action: ActionType,
    pub confirming_party: CantonId,
    /// Unix seconds when the confirmation contract was created on the ledger.
    /// 0 if the timestamp could not be resolved.
    #[serde(default)]
    pub created_at: i64,
    /// Unix seconds of the confirmation's `expiresAt`. 0 if unresolved.
    #[serde(default)]
    pub expires_at: i64,
}

/// A single confirmation of a domain-action proposal (governance-core
/// `Governance.Confirmation`). Unlike [`GovernanceConfirmation`] (which
/// backs core-self-management confirmations, each carrying its own
/// real inline `action`), the on-chain `Confirmation` contract carries no
/// action at all — only `actionProposalCid` and `actionLabel`, surfaced at
/// the parent [`DomainGovernanceAction`] level. There is no meaningful
/// per-confirmation action to serialize, so this type has no `action` field
/// rather than papering over the gap with a placeholder.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DomainConfirmation {
    pub contract_id: String,
    pub confirming_party: CantonId,
    /// Unix seconds when the confirmation contract was created on the ledger.
    /// 0 if the timestamp could not be resolved.
    #[serde(default)]
    pub created_at: i64,
    /// Unix seconds of the confirmation's `expiresAt`. 0 if unresolved.
    #[serde(default)]
    pub expires_at: i64,
}

/// A governance action with its confirmations, grouped by action hash
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceAction {
    /// Deterministic hash of the serialized action for grouping
    pub action_hash: String,
    /// The parsed action type
    pub action: ActionType,
    /// List of confirmations for this action
    pub confirmations: Vec<GovernanceConfirmation>,
    /// Number of confirmations
    pub confirmation_count: usize,
    /// Contract ids of the confirmations Execute submits
    pub executable_confirmation_cids: Vec<String>,
    /// Whether threshold is met for execution
    pub can_execute: bool,
    /// Unix seconds of the most recent confirmation (used for sorting in UI).
    #[serde(default)]
    pub last_confirmation_at: i64,
}

/// Response for governance confirmations endpoint
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceResponse {
    pub actions: Vec<GovernanceAction>,
    /// Opaque resume token for the next batch of proposals. Absent when the
    /// party has no more. Hand it back as `cursor` to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Pending domain action proposals (governance-core GovernableAction)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_actions: Vec<DomainGovernanceAction>,
    pub threshold: usize,
    /// The member party ID for the requesting party (used to identify own confirmations)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
    /// Current contract id of the active GovernanceRules contract for this
    /// party. The choice exercised when confirming an action
    /// is consuming, so this id changes after each confirm/execute — clients
    /// should use this field rather than a cached value to avoid
    /// `CONTRACT_NOT_FOUND` on stale ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_contract_id: Option<String>,
    /// True when the active governance-core rules contract is under an older
    /// package than configured (see `GovernanceState::out_of_date`).
    #[serde(default)]
    pub gov_core_out_of_date: bool,
    /// The package ref the rules contract actually lives under (for display
    /// in the out-of-date warning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gov_core_package_ref: Option<String>,
}

/// A decparty's reward automation as this node sees it.
///
/// It separates the three reasons a node assigns no reward coupons. This
/// participant may lack the rewards DAR, no delegation may be active, or the
/// delegation may name other assigners.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RewardStatusResponse {
    /// Whether this participant resolves `governance-rewards-automation-v1`.
    /// When false, no delegation was read: `delegation` is `None` and
    /// `beneficiaries` is empty.
    pub rewards_dar_resolvable: bool,
    /// The delegation the automation acts on: the newest, if several are active.
    pub delegation: Option<CouponReassignmentDelegationSummary>,
    /// This node's member party for the decparty.
    pub member_party_id: CantonId,
    /// Whether the delegation lists `member_party_id` as an assigner.
    pub member_is_assigner: bool,
    /// The delegation's beneficiary split, as the contract stores it.
    pub beneficiaries: Vec<RewardBeneficiary>,
}

// ============================================================================
// Audit Trail Types
// ============================================================================

/// Query parameters for the governance audit endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AuditLogQuery {
    /// Decentralized party ID to filter audit entries. Without it, the newest
    /// entries across every party are returned, and `limit` is capped at
    /// [`MAX_RECENT_AUDIT_LIMIT`].
    #[serde(default)]
    pub party_id: Option<CantonId>,
    /// Maximum number of entries to return (default 50)
    #[serde(default = "default_audit_limit")]
    pub limit: i64,
    /// Offset for pagination (default 0)
    #[serde(default)]
    pub offset: i64,
}

fn default_audit_limit() -> i64 {
    50
}

/// Most entries one cross-party audit read returns. The table grows with every
/// party on the node, so an unscoped read must not be able to ask for all of it.
pub const MAX_RECENT_AUDIT_LIMIT: i64 = 500;

// ============================================================================
// Chain Audit Trail Types
// ============================================================================

/// Which ledger events a chain-audit read returns.
///
/// `Governance` filters Canton-side to the governance packages and keeps only
/// proposals, confirmations, executions and their outcomes. `All` drops both
/// filters and returns every event the party witnesses, so a party whose
/// activity lives in its own application packages — an app or oracle party
/// that never deploys governance contracts — is not reported as having done
/// nothing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuditScope {
    #[default]
    Governance,
    All,
}

/// Query parameters for the on-chain governance audit endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ChainAuditQuery {
    /// Decentralized party ID to query chain events for
    pub party_id: CantonId,
    /// Maximum number of entries to return (default [`PAGE_SIZE`], capped at
    /// [`MAX_CHAIN_AUDIT_LIMIT`])
    #[serde(default = "default_chain_audit_limit")]
    pub limit: usize,
    /// Cursor: return only entries strictly older than this ledger offset.
    /// Pass the previous response's `next_before_offset` to get the next page.
    #[serde(default)]
    pub before_offset: Option<i64>,
    /// When true, fetches fresh data from Canton and updates cache
    #[serde(default)]
    pub refresh: bool,
    /// Which events to return: `governance` (default) or `all`.
    #[serde(default)]
    pub scope: AuditScope,
}

fn default_chain_audit_limit() -> usize {
    PAGE_SIZE as usize
}

/// Ceiling on a chain-audit page size.
///
/// `limit` arrives on the query string, so it is untrusted: left unbounded it
/// would drain the whole retained ledger into one response, and a value above
/// `i64::MAX` wraps negative when cast for SQLite — which reads a negative
/// `LIMIT` as no limit at all.
pub const MAX_CHAIN_AUDIT_LIMIT: usize = 1_000;

impl ChainAuditQuery {
    /// The requested page size, bounded by [`MAX_CHAIN_AUDIT_LIMIT`].
    pub fn clamped_limit(&self) -> usize {
        self.limit.min(MAX_CHAIN_AUDIT_LIMIT)
    }
}

/// Build a [`ChainAuditEntry`] wire DTO from a cached DB row.
///
/// A free function rather than `impl From` because `ChainAuditEntry` now lives
/// in the `common` crate; the orphan rule forbids implementing the foreign
/// `From` trait for a foreign type here. Mirrors [`permission_from_proto`].
pub fn chain_audit_entry_from_row(row: crate::db::rows::ChainAuditCacheRow) -> ChainAuditEntry {
    ChainAuditEntry {
        offset: row.offset,
        timestamp: row.timestamp,
        event_type: row.event_type,
        contract_id: row.contract_id,
        template_id: row.template_id,
        package_id: row.package_id,
        governance_type: row.governance_type,
        action_summary: row.action_summary,
        choice: row.choice,
        acting_parties: serde_json::from_str(&row.acting_parties).unwrap_or_default(),
        update_id: row.update_id,
        details: serde_json::from_str(&row.details).unwrap_or(serde_json::Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::test_party;
    use super::*;

    /// `/governance/confirmations` is deserialized by the integration-test
    /// harness, so the response has to survive a round trip in the shape the
    /// server actually emits — which omits every `skip_serializing_if` field.
    /// Without a matching `default` those come back as "missing field" errors.
    #[test]
    fn governance_response_round_trips_with_every_optional_field_omitted() -> anyhow::Result<()> {
        let response = GovernanceResponse {
            next_cursor: None,
            actions: vec![GovernanceAction {
                action_hash: "hash".to_owned(),
                action: ActionType::GovernanceSetThreshold { new_threshold: 2 },
                confirmations: vec![GovernanceConfirmation {
                    contract_id: "00conf".to_owned(),
                    action: ActionType::GovernanceSetThreshold { new_threshold: 2 },
                    confirming_party: test_party("m1")?,
                    created_at: 0,
                    expires_at: 0,
                }],
                confirmation_count: 1,
                executable_confirmation_cids: Vec::new(),
                can_execute: false,
                last_confirmation_at: 0,
            }],
            domain_actions: vec![DomainGovernanceAction {
                proposal_cid: "00prop".to_owned(),
                action_label: "SetThreshold".to_owned(),
                description: None,
                confirmations: Vec::new(),
                confirmation_count: 0,
                executable_confirmation_cids: Vec::new(),
                can_execute: false,
                orphaned: false,
                transfer_details: None,
                accept_transfer_details: None,
                service_request_details: None,
                proposer: None,
                proposer_not_authorized: false,
                created_at: None,
                proposal_template_id: None,
                proposal_payload: None,
            }],
            threshold: 2,
            member_party_id: None,
            rules_contract_id: None,
            gov_core_out_of_date: false,
            gov_core_package_ref: None,
        };

        let json = serde_json::to_string(&response)?;
        assert!(
            !json.contains("member_party_id")
                && !json.contains("\"proposer\"")
                && !json.contains("proposal_template_id")
                && !json.contains("proposal_payload"),
            "optional fields must be omitted on the wire: {json}"
        );

        let back: GovernanceResponse = serde_json::from_str(&json)?;
        assert_eq!(back.threshold, 2);
        assert_eq!(back.member_party_id, None);
        assert_eq!(
            back.domain_actions.first().map(|a| a.action_label.as_str()),
            Some("SetThreshold")
        );
        assert_eq!(
            back.actions
                .first()
                .and_then(|a| a.confirmations.first())
                .map(|c| c.confirming_party.clone()),
            Some(test_party("m1")?)
        );
        Ok(())
    }

    /// A domain-action confirmation (`DomainConfirmation`, backing
    /// governance-core `Confirmation` contracts) has no real inline action —
    /// only the parent `DomainGovernanceAction.action_label` describes it —
    /// so it must never serialize an `"action"` key at all, placeholder or
    /// otherwise. The sibling self-management confirmation
    /// (`GovernanceConfirmation`) genuinely does carry its own action and
    /// must keep serializing the real one.
    #[test]
    fn domain_confirmation_omits_action_self_confirmation_keeps_it() -> anyhow::Result<()> {
        let response = GovernanceResponse {
            next_cursor: None,
            actions: vec![GovernanceAction {
                action_hash: "hash".to_owned(),
                action: ActionType::GovernanceSetThreshold { new_threshold: 7 },
                confirmations: vec![GovernanceConfirmation {
                    contract_id: "self-conf".to_owned(),
                    action: ActionType::GovernanceSetThreshold { new_threshold: 7 },
                    confirming_party: test_party("m1")?,
                    created_at: 0,
                    expires_at: 0,
                }],
                confirmation_count: 1,
                executable_confirmation_cids: Vec::new(),
                can_execute: false,
                last_confirmation_at: 0,
            }],
            domain_actions: vec![DomainGovernanceAction {
                proposal_cid: "00prop".to_owned(),
                action_label: "WithdrawPending".to_owned(),
                description: None,
                confirmations: vec![DomainConfirmation {
                    contract_id: "domain-conf".to_owned(),
                    confirming_party: test_party("m2")?,
                    created_at: 0,
                    expires_at: 0,
                }],
                confirmation_count: 1,
                executable_confirmation_cids: Vec::new(),
                can_execute: false,
                orphaned: false,
                transfer_details: None,
                accept_transfer_details: None,
                service_request_details: None,
                proposer: None,
                proposer_not_authorized: false,
                created_at: None,
                proposal_template_id: None,
                proposal_payload: None,
            }],
            threshold: 2,
            member_party_id: None,
            rules_contract_id: None,
            gov_core_out_of_date: false,
            gov_core_package_ref: None,
        };

        let value = serde_json::to_value(&response)?;

        let domain_confirmation = &value["domain_actions"][0]["confirmations"][0];
        assert!(
            domain_confirmation.get("action").is_none(),
            "domain confirmation must not carry an action field: {domain_confirmation}"
        );
        // Sanity: not just an empty object — the fields we do expect are there.
        assert_eq!(domain_confirmation["contract_id"], "domain-conf");

        let self_confirmation = &value["actions"][0]["confirmations"][0];
        assert_eq!(
            self_confirmation["action"]["type"], "governance_set_threshold",
            "self-management confirmation must keep its real action: {self_confirmation}"
        );
        assert_eq!(self_confirmation["action"]["new_threshold"], 7);

        Ok(())
    }

    /// `ServiceRequestDetails` sets exactly one of `user` / `provider`; the
    /// unset one is omitted and must deserialize back as `None`.
    #[test]
    fn service_request_details_round_trips_with_one_side_unset() -> anyhow::Result<()> {
        let details = ServiceRequestDetails {
            operator: test_party("op")?,
            user: None,
            provider: Some(test_party("prov")?),
        };
        let json = serde_json::to_string(&details)?;
        let back: ServiceRequestDetails = serde_json::from_str(&json)?;
        assert_eq!(back.user, None);
        assert_eq!(back.provider, Some(test_party("prov")?));
        Ok(())
    }
}
