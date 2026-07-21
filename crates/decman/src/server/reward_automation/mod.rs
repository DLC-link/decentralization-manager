// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! CIP-104 Mode A reward-assignment automation.
//!
//! A decparty earns traffic-based app rewards as `RewardCouponV2` coupons that
//! carry `beneficiary = None` and expire unclaimed unless automation assigns
//! them to the decparty's governance-configured split. This module holds the
//! per-node read side of that automation:
//!
//! * [`active_created_records`] — the one shared **decoded** ACS read. Unlike
//!   `queries::query_contracts_by_template` (cid + base64 blob, no fields) and
//!   `queries::get_contracts` (metadata only), this issues a direct
//!   `StateServiceClient` `GetActiveContracts` — modeled on
//!   `queries::fetch_proposal_infos` — and returns the decoded create-arguments
//!   `Record` (template reads) or the decoded interface-view `Record`
//!   (interface reads).
//! * [`OnLedgerSplitSource`] — reads the singleton `RewardSplitConfig` for a
//!   decparty via a [`SplitSource`], defending the single-config invariant.
//! * [`unassigned_coupons`] — reads the decparty's unassigned `RewardCoupon`
//!   interface views.
//!
//! The pure record decoders ([`parse_split_record`]) are unit-tested here; the
//! gRPC reads are exercised by the devnet integration test.

use std::collections::HashMap;

use anyhow::{Context, anyhow};
use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
    Identifier, InterfaceFilter, Record, TemplateFilter, WildcardFilter, cumulative_filter,
    get_active_contracts_response::ContractEntry, value,
};
use chrono::{DateTime, Utc};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    utils,
};

use std::time::Duration;

use super::AppState;
use super::handlers::{
    execute_confirm_action, get_party_credentials, packages, resolve_active_governance_rules,
    submit_proposal,
};
use super::queries::get_governance_confirmations;
use super::types::{
    ActionType, ConfirmActionRequest, DomainGovernanceAction, GovernanceType, ProposalType,
    RewardBeneficiary,
};

// ============================================================================
// Record field extraction (mirrors queries.rs `field_*` helpers; the originals
// are module-private, so we follow the same `value::Sum` matching pattern here
// rather than widening their visibility).
// ============================================================================

/// Return the decoded `value::Sum` for `label`, if present.
fn record_field<'a>(rec: &'a Record, label: &str) -> Option<&'a value::Sum> {
    rec.fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| v.sum.as_ref())
}

/// Read a `Party` field and parse it into a [`CantonId`].
fn field_party_id(rec: &Record, label: &str) -> anyhow::Result<CantonId> {
    match record_field(rec, label) {
        Some(value::Sum::Party(p)) => p
            .parse::<CantonId>()
            .with_context(|| format!("field `{label}`: invalid party id `{p}`")),
        _ => Err(anyhow!("field `{label}`: expected a Party value")),
    }
}

/// Read a `Numeric` field and parse it into a [`DamlDecimal`] (exact fixed-point).
fn field_decimal(rec: &Record, label: &str) -> anyhow::Result<DamlDecimal> {
    match record_field(rec, label) {
        Some(value::Sum::Numeric(n)) => DamlDecimal::parse(n)
            .map_err(|e| anyhow!("field `{label}`: invalid decimal `{n}`: {e}")),
        _ => Err(anyhow!("field `{label}`: expected a Numeric value")),
    }
}

/// Read a DAML `Time` field (encoded as microseconds since the epoch) into a
/// UTC timestamp.
fn field_time(rec: &Record, label: &str) -> anyhow::Result<DateTime<Utc>> {
    let micros = match record_field(rec, label) {
        Some(value::Sum::Timestamp(t)) => *t,
        _ => return Err(anyhow!("field `{label}`: expected a Time value")),
    };
    DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| anyhow!("field `{label}`: timestamp {micros} micros is out of range"))
}

/// Read a `ContractId` field as its contract-id string. On the wire a
/// `ContractId` is `value::Sum::ContractId(String)`.
fn field_contract_id(rec: &Record, label: &str) -> anyhow::Result<String> {
    match record_field(rec, label) {
        Some(value::Sum::ContractId(c)) => Ok(c.clone()),
        _ => Err(anyhow!("field `{label}`: expected a ContractId value")),
    }
}

/// Read a list-of-`ContractId` field as contract-id strings.
fn field_contract_id_list(rec: &Record, label: &str) -> anyhow::Result<Vec<String>> {
    let list = match record_field(rec, label) {
        Some(value::Sum::List(l)) => l,
        _ => return Err(anyhow!("field `{label}`: expected a List value")),
    };
    list.elements
        .iter()
        .map(|elem| match elem.sum.as_ref() {
            Some(value::Sum::ContractId(c)) => Ok(c.clone()),
            _ => Err(anyhow!("field `{label}`: element is not a ContractId")),
        })
        .collect()
}

/// Return true iff `label` is an `Optional` field carrying `None`.
///
/// A missing field, or a non-optional value, returns false — the caller then
/// treats the contract as *not* unassigned (fail-safe: never propose against a
/// coupon we can't confirm is unassigned).
fn field_optional_is_none(rec: &Record, label: &str) -> bool {
    matches!(record_field(rec, label), Some(value::Sum::Optional(opt)) if opt.value.is_none())
}

// ============================================================================
// Shared decoded ACS read
// ============================================================================

/// A single decoded `GetActiveContracts` read.
///
/// For `interface_view = false` this uses a `TemplateFilter` (or, under
/// `test_mode`, a `WildcardFilter` with in-memory template matching, since mock
/// auth lacks `TemplateFilter` permission) and returns each created event's
/// `create_arguments` `Record`. For `interface_view = true` it uses an
/// `InterfaceFilter { include_interface_view: true }` and returns the decoded
/// interface-view `Record`. Field labels are populated because the request is
/// `verbose`.
///
/// Modeled on `queries::fetch_proposal_infos`.
// The full filter descriptor (package/module/entity + template-vs-interface) is
// intentionally passed positionally so this stays the single shared read for
// every reward-automation query.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn active_created_records(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    package_id: &str,
    module: &str,
    entity: &str,
    interface_view: bool,
) -> anyhow::Result<Vec<(String, Record)>> {
    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let identifier_filter = if interface_view {
        cumulative_filter::IdentifierFilter::InterfaceFilter(InterfaceFilter {
            interface_id: Some(Identifier {
                package_id: package_id.to_string(),
                module_name: module.to_string(),
                entity_name: entity.to_string(),
            }),
            include_interface_view: true,
            include_created_event_blob: false,
        })
    } else if test_mode {
        cumulative_filter::IdentifierFilter::WildcardFilter(WildcardFilter {
            include_created_event_blob: false,
        })
    } else {
        cumulative_filter::IdentifierFilter::TemplateFilter(TemplateFilter {
            template_id: Some(Identifier {
                package_id: package_id.to_string(),
                module_name: module.to_string(),
                entity_name: entity.to_string(),
            }),
            include_created_event_blob: false,
        })
    };

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(identifier_filter),
            }],
        },
    );

    let acs_request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: true,
        }),
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    let mut out = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            if interface_view {
                let Some(view) = created.interface_views.iter().find(|v| {
                    v.interface_id
                        .as_ref()
                        .is_some_and(|id| id.module_name == module && id.entity_name == entity)
                }) else {
                    continue;
                };
                if let Some(rec) = view.view_value.clone() {
                    out.push((created.contract_id.clone(), rec));
                }
            } else {
                // Wildcard (test mode) returns every template; keep only the
                // requested one. Match on module/entity — the package alias
                // (`#…`) is resolved to a concrete hash on the wire.
                if test_mode
                    && !created
                        .template_id
                        .as_ref()
                        .is_some_and(|t| t.module_name == module && t.entity_name == entity)
                {
                    continue;
                }
                if let Some(rec) = created.create_arguments.clone() {
                    out.push((created.contract_id.clone(), rec));
                }
            }
        }
    }

    Ok(out)
}

// ============================================================================
// RewardSplitConfig (the on-ledger split)
// ============================================================================

/// Decode a list of `{ beneficiary : Party, percentage : Numeric }` records at
/// `label` into [`RewardBeneficiary`]s. Shared by `RewardSplitConfig`
/// (`beneficiaries`) and `AssignRewardBeneficiaries` (`newBeneficiaries`), which
/// carry the identical element shape.
fn parse_beneficiary_list(rec: &Record, label: &str) -> anyhow::Result<Vec<RewardBeneficiary>> {
    let list = match record_field(rec, label) {
        Some(value::Sum::List(l)) => l,
        _ => return Err(anyhow!("field `{label}`: expected a beneficiary List")),
    };

    let mut out = Vec::with_capacity(list.elements.len());
    for elem in &list.elements {
        let inner = match elem.sum.as_ref() {
            Some(value::Sum::Record(r)) => r,
            _ => return Err(anyhow!("field `{label}`: element is not a record")),
        };
        out.push(RewardBeneficiary {
            beneficiary: field_party_id(inner, "beneficiary")?,
            percentage: field_decimal(inner, "percentage")?,
        });
    }
    Ok(out)
}

/// Decode a `RewardSplitConfig` create-arguments `Record` into the configured
/// split. Reads the `beneficiaries` field — a list of
/// `{ beneficiary : Party, percentage : Numeric }` records.
pub(crate) fn parse_split_record(rec: &Record) -> anyhow::Result<Vec<RewardBeneficiary>> {
    parse_beneficiary_list(rec, "beneficiaries")
}

/// The effective reward split for a decparty, read from its on-ledger
/// `RewardSplitConfig`, or `None` when there is no config (i.e. the automation
/// is not enabled for that decparty). Defends the keyless-singleton invariant.
///
/// This is the **single swap point** for the split source: if a shared
/// reward-config template ships later, only this function body changes.
/// (Kept as a plain async fn — the codebase deliberately avoids `async-trait`,
/// and there is one source today, so a trait would be premature.)
pub(crate) async fn effective_split(
    config: &NodeConfig,
    packages: &PackageConfig,
    test_mode: bool,
    decparty: &CantonId,
    token: &str,
) -> anyhow::Result<Option<Vec<RewardBeneficiary>>> {
    let Some(package_id) = packages.governance_rewards.as_deref() else {
        return Ok(None);
    };

    let records = active_created_records(
        config,
        decparty,
        Some(token.to_string()),
        test_mode,
        package_id,
        "Governance.Rewards.RewardSplitConfig",
        "RewardSplitConfig",
        false,
    )
    .await?;

    // Defend the keyless-singleton invariant: keep only configs whose
    // `governanceParty` is this decparty.
    let mut matching: Vec<&Record> = records
        .iter()
        .filter(|(_, rec)| field_party_id(rec, "governanceParty").ok().as_ref() == Some(decparty))
        .map(|(_, rec)| rec)
        .collect();

    match matching.len() {
        0 => Ok(None),
        1 => Ok(Some(parse_split_record(matching.remove(0))?)),
        n => {
            tracing::warn!("ambiguous RewardSplitConfig for {decparty}: {n} active — refusing");
            Err(anyhow!(
                "ambiguous RewardSplitConfig: {n} active — refusing"
            ))
        }
    }
}

// ============================================================================
// Unassigned reward coupons
// ============================================================================

/// A decparty's unassigned reward coupon (`RewardCoupon` interface view).
pub(crate) struct CouponInfo {
    pub cid: String,
    /// Populated for operator logging and the devnet IT's assertions; the
    /// batching logic itself keys off `cid` + `expires_at` only.
    #[allow(dead_code)]
    pub provider: CantonId,
    /// See `provider` — surfaced for logging/IT, not read by batching.
    #[allow(dead_code)]
    pub amount: DamlDecimal,
    pub expires_at: DateTime<Utc>,
}

/// Read every reward coupon for `decparty` that is still unassigned
/// (`provider == decparty` and `beneficiary` is `None`).
///
/// Uses the `RewardCoupon` interface view (`#splice-api-reward-assignment-v1`);
/// on devnet the concrete implementer is `RewardCouponV2`.
pub(crate) async fn unassigned_coupons(
    config: &NodeConfig,
    decparty: &CantonId,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<Vec<CouponInfo>> {
    let _ = packages; // the interface is resolved by name-alias, not PackageConfig.
    let records = active_created_records(
        config,
        decparty,
        token,
        test_mode,
        "#splice-api-reward-assignment-v1",
        "Splice.Api.RewardAssignmentV1",
        "RewardCoupon",
        true,
    )
    .await?;

    let mut out = Vec::new();
    for (cid, rec) in records {
        let provider = field_party_id(&rec, "provider")?;
        if &provider != decparty {
            continue;
        }
        if !field_optional_is_none(&rec, "beneficiary") {
            continue;
        }
        out.push(CouponInfo {
            cid,
            provider,
            amount: field_decimal(&rec, "amount")?,
            expires_at: field_time(&rec, "expiresAt")?,
        });
    }
    Ok(out)
}

// ============================================================================
// Pending AssignRewardBeneficiaries proposal + auto-confirmation policy
// ============================================================================

/// A pending `AssignRewardBeneficiaries` proposal, read back from the ledger so
/// the confirmer can validate it against the configured split before adding this
/// node's confirmation. Populated by [`read_all_pending_assigns`].
#[derive(Clone, Debug)]
pub(crate) struct PendingAssign {
    /// Mirrors the `DomainGovernanceAction` cid so a `PendingAssign` is
    /// self-describing; the confirmer keys off the action's cid, not this.
    #[allow(dead_code)]
    pub proposal_cid: String,
    /// The `governanceParty` the proposal is scoped to. The confirmer verifies
    /// this equals the decparty locally, so the security-critical invariant does
    /// not rely on ACS visibility alone (M-6).
    pub governance_party: CantonId,
    pub primary_coupon: String,
    pub additional_coupons: Vec<String>,
    pub new_beneficiaries: Vec<RewardBeneficiary>,
}

/// True iff `proposed` and `configured` describe the same split: equal length,
/// and every configured `(beneficiary, percentage)` has a matching proposed
/// entry. Beneficiaries compare by [`CantonId`] equality and percentages by
/// **exact [`DamlDecimal`]** equality — no float tolerance, mirroring the DAML
/// `total == 1.0` guard and `validate_reward_beneficiaries` in `server::types`.
pub(crate) fn split_matches(
    proposed: &[RewardBeneficiary],
    configured: &[RewardBeneficiary],
) -> bool {
    if proposed.len() != configured.len() {
        return false;
    }
    configured.iter().all(|c| {
        proposed
            .iter()
            .any(|p| p.beneficiary == c.beneficiary && p.percentage == c.percentage)
    })
}

/// Default-deny auto-confirmation policy with an allowlist of exactly one action
/// label. Returns true **iff** the action is `AssignRewardBeneficiaries`, its
/// proposed split matches the configured one, and every target coupon is still
/// unassigned. Any other action label is refused (correctness comes from this
/// check, never from trusting the proposer).
pub(crate) fn is_confirmable(
    action_label: &str,
    proposal: &PendingAssign,
    configured: &[RewardBeneficiary],
    coupons_unassigned: bool,
) -> bool {
    action_label == "AssignRewardBeneficiaries"
        && split_matches(&proposal.new_beneficiaries, configured)
        && coupons_unassigned
}

// ============================================================================
// Parse-back of a pending AssignRewardBeneficiaries proposal
// ============================================================================

/// Decode an `AssignRewardBeneficiaries` create-arguments `Record` into a
/// [`PendingAssign`]. Field order (verified against the M1 template):
/// `governanceParty, proposer, primaryCoupon, additionalCoupons,
/// newBeneficiaries`. `primaryCoupon` / `additionalCoupons` are `ContractId`s;
/// `newBeneficiaries` reuses the shared beneficiary-list decoder.
fn parse_assign_record(cid: &str, rec: &Record) -> anyhow::Result<PendingAssign> {
    Ok(PendingAssign {
        proposal_cid: cid.to_string(),
        governance_party: field_party_id(rec, "governanceParty")?,
        primary_coupon: field_contract_id(rec, "primaryCoupon")?,
        additional_coupons: field_contract_id_list(rec, "additionalCoupons")?,
        new_beneficiaries: parse_beneficiary_list(rec, "newBeneficiaries")?,
    })
}

/// Read the pending `AssignRewardBeneficiaries` proposal identified by
/// `proposal_cid`, decoding its target coupons and proposed split. Returns
/// `Ok(None)` if the governance-rewards package is unconfigured or the proposal
/// is no longer active (already executed/expired).
pub(crate) async fn read_all_pending_assigns(
    config: &NodeConfig,
    decparty: &CantonId,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<HashMap<String, PendingAssign>> {
    let Some(package_id) = packages.governance_rewards.as_deref() else {
        return Ok(HashMap::new());
    };

    let records = active_created_records(
        config,
        decparty,
        token,
        test_mode,
        package_id,
        "Governance.Rewards.AssignRewardBeneficiaries",
        "AssignRewardBeneficiaries",
        false,
    )
    .await?;

    // One ACS scan per tick; callers look up by cid instead of re-scanning per
    // pending proposal.
    let mut out = HashMap::with_capacity(records.len());
    for (cid, rec) in &records {
        out.insert(cid.clone(), parse_assign_record(cid, rec)?);
    }
    Ok(out)
}

// ============================================================================
// Proposer role
// ============================================================================

/// Total lifetime of a `RewardCouponV2` coupon (spec §1: 36h TTL). Used to
/// derive a coupon's age from its `expiresAt`, since the interface view exposes
/// no `createdAt`.
const COUPON_TTL: chrono::Duration = chrono::Duration::hours(36);

/// Select the coupons to assign this tick (pure). Keeps a coupon iff:
///   * it is old enough — its age (`now - (expiresAt - COUPON_TTL)`) is past
///     `watermark`, so freshly-earned coupons get first refusal by any other
///     collection path before we sweep them (spec §9/§11); AND
///   * enough time remains before expiry to mint after assigning —
///     `expiresAt - now >= minting_margin`.
///
/// Survivors are ordered most-urgent-first (ascending `expiresAt`) and truncated
/// to `max_batch`; the coupon cids are returned.
pub(crate) fn select_batch(
    coupons: &[CouponInfo],
    now: DateTime<Utc>,
    watermark: chrono::Duration,
    minting_margin: chrono::Duration,
    max_batch: usize,
) -> Vec<String> {
    let mut selected: Vec<&CouponInfo> = coupons
        .iter()
        .filter(|c| {
            let age = now - (c.expires_at - COUPON_TTL);
            let remaining = c.expires_at - now;
            age >= watermark && remaining >= minting_margin
        })
        .collect();
    selected.sort_by_key(|c| c.expires_at);
    selected
        .into_iter()
        .take(max_batch)
        .map(|c| c.cid.clone())
        .collect()
}

/// One proposer tick for a decparty: read its unassigned coupons, drop any
/// already targeted by an in-flight proposal (`covered_coupons`), select a ripe
/// batch, and — if non-empty — propose `AssignRewardBeneficiaries` for the
/// configured `split`. An empty batch (nothing ripe, or all covered) is a no-op.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_proposer_once(
    config: &NodeConfig,
    decparty: &CantonId,
    member_party_id: &CantonId,
    token: &str,
    rules_contract_id: &str,
    split: &[RewardBeneficiary],
    test_mode: bool,
    packages: &PackageConfig,
    covered_coupons: &std::collections::HashSet<String>,
) -> anyhow::Result<()> {
    // Batch policy constants (spec §9 proposer step 2, §11 TTL/margin/cap):
    //   * watermark 6h — leave freshly-earned coupons to any other collection
    //     path first; only sweep what remains unassigned after 6h.
    //   * minting_margin 2h — refuse coupons too close to their 36h expiry to
    //     still be minted after the assignment lands.
    //   * max_batch 50 — a conservative per-tick cap so a single propose command
    //     stays well within ledger limits.
    const WATERMARK: chrono::Duration = chrono::Duration::hours(6);
    const MINTING_MARGIN: chrono::Duration = chrono::Duration::hours(2);
    const MAX_BATCH: usize = 50;

    let coupons: Vec<CouponInfo> = unassigned_coupons(
        config,
        decparty,
        Some(token.to_string()),
        test_mode,
        packages,
    )
    .await?
    .into_iter()
    .filter(|c| !covered_coupons.contains(&c.cid))
    .collect();

    let batch = select_batch(&coupons, Utc::now(), WATERMARK, MINTING_MARGIN, MAX_BATCH);
    if batch.is_empty() {
        return Ok(());
    }

    let proposal = ProposalType::AssignRewardBeneficiaries {
        primary_coupon: batch[0].clone(),
        additional_coupons: batch[1..].to_vec(),
        new_beneficiaries: split.to_vec(),
    };

    tracing::info!(
        %decparty,
        batch_size = batch.len(),
        coupons = ?batch,
        "reward automation: proposing AssignRewardBeneficiaries",
    );

    // `submit_proposal` ignores its `rules_contract_id` arg for creates (it
    // resolves the package ref itself), so pass an empty string here.
    let cid = submit_proposal(
        config,
        decparty,
        "",
        &proposal,
        token,
        member_party_id,
        packages,
    )
    .await?;

    // Immediately cast the proposer's own confirmation. Without this the
    // proposal has zero confirmations and `get_governance_confirmations` hides
    // it from every confirmer (including this node) — so it would never be
    // visible and never reach threshold (C-1 liveness). The confirming choice
    // needs the real governance rules cid, unlike the create above.
    submit_confirmation(
        config,
        decparty,
        rules_contract_id,
        &cid,
        token,
        member_party_id,
        packages,
    )
    .await?;

    tracing::info!(
        %decparty,
        proposal_cid = %cid,
        "reward automation: proposed + self-confirmed AssignRewardBeneficiaries",
    );

    Ok(())
}

// ============================================================================
// Confirmer role
// ============================================================================

/// Submit this node's confirmation for a `CoreDomain` governance proposal.
///
/// Builds the SAME `CoreDomain` [`ConfirmActionRequest`] the confirmer uses
/// inline — `execute_confirm_action`'s CoreDomain branch derives the choice arg
/// from `proposal_cid`, so `action` is an inert placeholder (mirrors the
/// frontend's `governance_set_threshold { new_threshold: 0 }` placeholder in
/// NotificationsView.tsx) — and calls [`execute_confirm_action`].
///
/// Used both by the proposer (to immediately cast its own bootstrapping vote so
/// the proposal is visible to confirmers — `get_governance_confirmations` hides
/// zero-confirmation proposals) and by the confirmer.
async fn submit_confirmation(
    config: &NodeConfig,
    decparty: &CantonId,
    rules_contract_id: &str,
    proposal_cid: &str,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> anyhow::Result<()> {
    let req = ConfirmActionRequest {
        party_id: decparty.clone(),
        rules_contract_id: rules_contract_id.to_string(),
        action: ActionType::GovernanceSetThreshold { new_threshold: 0 },
        governance_type: GovernanceType::CoreDomain,
        proposal_cid: Some(proposal_cid.to_string()),
    };
    execute_confirm_action(config, &req, token, member_party_id, packages).await?;
    Ok(())
}

/// True iff `member` already appears among `action`'s confirmations, so this
/// node does not double-confirm a proposal it has already signed.
pub(crate) fn already_confirmed_by(action: &DomainGovernanceAction, member: &CantonId) -> bool {
    action
        .confirmations
        .iter()
        .any(|c| &c.confirming_party == member)
}

/// One confirmer tick for a decparty. For each pending
/// `AssignRewardBeneficiaries` proposal this node has not yet confirmed, read it
/// back from the ledger, validate its proposed split against the on-ledger
/// `split` and that every target coupon is still unassigned, and — only then —
/// add this node's confirmation. Correctness comes from this check, never from
/// trusting the proposer (default-deny; spec §5, §12).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_confirmer_once(
    data: &actix_web::web::Data<AppState>,
    config: &NodeConfig,
    decparty: &CantonId,
    member_party_id: &CantonId,
    token: &str,
    rules_contract_id: &str,
    split: &[RewardBeneficiary],
    domain: &[DomainGovernanceAction],
    pending: &std::collections::HashMap<String, PendingAssign>,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<()> {
    // Held for the (deferred) first-wins execute path; see TODO(M4) below.
    let _ = data;

    // Fetch the unassigned set ONCE this tick; reused to recheck every
    // proposal's target coupons (do not re-query per proposal).
    let live: std::collections::HashSet<String> = unassigned_coupons(
        config,
        decparty,
        Some(token.to_string()),
        test_mode,
        packages,
    )
    .await?
    .into_iter()
    .map(|c| c.cid)
    .collect();

    for a in domain {
        if a.action_label != "AssignRewardBeneficiaries"
            || a.orphaned
            || already_confirmed_by(a, member_party_id)
        {
            continue;
        }

        let Some(pa) = pending.get(&a.proposal_cid) else {
            continue;
        };

        // Verify the proposal is scoped to this decparty locally, rather than
        // relying on ACS visibility alone (M-6, security-critical path).
        if pa.governance_party != *decparty {
            tracing::warn!(
                %decparty,
                proposal_cid = %a.proposal_cid,
                governance_party = %pa.governance_party,
                "reward automation: refusing to confirm AssignRewardBeneficiaries \
                 (governanceParty does not match decparty)",
            );
            continue;
        }

        let coupons_ok = std::iter::once(&pa.primary_coupon)
            .chain(&pa.additional_coupons)
            .all(|c| live.contains(c));

        if is_confirmable(&a.action_label, pa, split, coupons_ok) {
            submit_confirmation(
                config,
                decparty,
                rules_contract_id,
                &a.proposal_cid,
                token,
                member_party_id,
                packages,
            )
            .await?;
            tracing::info!(
                %decparty,
                proposal_cid = %a.proposal_cid,
                "reward automation: confirmed AssignRewardBeneficiaries",
            );
        } else {
            tracing::warn!(
                %decparty,
                proposal_cid = %a.proposal_cid,
                "reward automation: refusing to confirm AssignRewardBeneficiaries \
                 (split mismatch or a target coupon is no longer unassigned)",
            );
        }

        // TODO(M4): optional first-wins execute when `a.can_execute` — left out
        // to keep scope tight; execute stays human-driven for now.
    }

    Ok(())
}

// ============================================================================
// Background loop + registration
// ============================================================================

/// Per-node background loop: every `reward_automation_interval_secs`, run the
/// proposer + confirmer once for each decparty this node holds credentials for.
/// Enablement is on-ledger — a decparty with no `RewardSplitConfig` is skipped.
pub(crate) async fn run_reward_automation_loop(data: actix_web::web::Data<AppState>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(
        data.config.reward_automation_interval_secs,
    ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let parties: Vec<CantonId> = data
            .party_credentials
            .read()
            .await
            .iter()
            .map(|p| p.dec_party_id.clone())
            .collect();
        for decparty in parties {
            if let Err(e) = run_once_for_party(&data, &decparty).await {
                tracing::warn!(%decparty, error = %e, "reward automation tick failed");
            }
        }
    }
}

/// One proposer + confirmer pass for a single decparty. No-op unless the
/// decparty has an on-ledger `RewardSplitConfig` (the enablement signal).
async fn run_once_for_party(
    data: &actix_web::web::Data<AppState>,
    decparty: &CantonId,
) -> anyhow::Result<()> {
    let pkgs = packages();
    let Some((token, member)) = get_party_credentials(data, decparty).await else {
        return Ok(());
    };
    // Enablement: exactly one RewardSplitConfig => on; none => off; >1 => Err.
    let Some(split) =
        effective_split(&data.config, &pkgs, data.test_mode, decparty, &token).await?
    else {
        return Ok(());
    };
    // Governance rules cid + governance threshold (NOT the topology threshold).
    let (rules_cid, threshold) = resolve_active_governance_rules(
        &data.config,
        decparty,
        Some(token.clone()),
        data.test_mode,
        &pkgs,
    )
    .await?;
    // Fetch pending governance actions once; shared by dedupe + the confirmer.
    let (_, domain) = get_governance_confirmations(
        &data.config,
        decparty,
        threshold,
        Some(token.clone()),
        data.test_mode,
        &pkgs,
    )
    .await?;
    // All in-flight AssignRewardBeneficiaries proposals for this decparty, read
    // ONCE (indexed by cid) and shared by the proposer's dedupe + the confirmer,
    // instead of a full ACS scan per pending proposal (M-5).
    let pending = read_all_pending_assigns(
        &data.config,
        decparty,
        Some(token.clone()),
        data.test_mode,
        &pkgs,
    )
    .await?;
    // Coupons already covered by an in-flight proposal => dedupe the proposer.
    let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for pa in pending.values() {
        covered.insert(pa.primary_coupon.clone());
        covered.extend(pa.additional_coupons.iter().cloned());
    }
    // A transient propose error must not skip confirming already-pending
    // proposals this tick (M-1): log and continue to the confirmer.
    if let Err(e) = run_proposer_once(
        &data.config,
        decparty,
        &member,
        &token,
        &rules_cid,
        &split,
        data.test_mode,
        &pkgs,
        &covered,
    )
    .await
    {
        tracing::warn!(%decparty, error = %e, "reward automation: proposer tick failed");
    }
    run_confirmer_once(
        data,
        &data.config,
        decparty,
        &member,
        &token,
        &rules_cid,
        &split,
        &domain,
        &pending,
        data.test_mode,
        &pkgs,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::types::GovernanceConfirmation;
    use canton_proto_rs::com::daml::ledger::api::v2::{List, RecordField, Value};

    fn value(sum: value::Sum) -> Value {
        Value { sum: Some(sum) }
    }

    fn field(label: &str, sum: value::Sum) -> RecordField {
        RecordField {
            label: label.to_string(),
            value: Some(value(sum)),
        }
    }

    fn record(fields: Vec<RecordField>) -> Record {
        Record {
            record_id: None,
            fields,
        }
    }

    fn party(p: &str) -> value::Sum {
        value::Sum::Party(p.to_string())
    }

    fn numeric(n: &str) -> value::Sum {
        value::Sum::Numeric(n.to_string())
    }

    fn beneficiary_record(p: &str, pct: &str) -> Value {
        value(value::Sum::Record(record(vec![
            field("beneficiary", party(p)),
            field("percentage", numeric(pct)),
        ])))
    }

    fn contract_id(c: &str) -> value::Sum {
        value::Sum::ContractId(c.to_string())
    }

    /// Test-only helper mirroring `server::types` tests: builds a
    /// [`RewardBeneficiary`] from a Canton-ID prefix + a decimal percentage
    /// string. A fixed valid namespace keeps party ids parseable; distinct
    /// prefixes yield distinct parties.
    fn rb(prefix: &str, pct: &str) -> RewardBeneficiary {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        RewardBeneficiary {
            beneficiary: CantonId::parse(&format!("{prefix}::{ns}")).expect("valid canton id"),
            percentage: pct.parse().expect("valid decimal"),
        }
    }

    // Valid Canton party ids: `prefix::<multihash>` where the namespace is a
    // 34-byte (68-hex-char) SHA-256 multihash (`1220` + 64 hex chars).
    const GOV: &str = "gov::12200000000000000000000000000000000000000000000000000000000000000000";
    const ALICE: &str =
        "alice::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "bob::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn parse_split_record_reads_beneficiaries() {
        let rec = record(vec![
            field("governanceParty", party(GOV)),
            field(
                "beneficiaries",
                value::Sum::List(List {
                    elements: vec![
                        beneficiary_record(ALICE, "0.8"),
                        beneficiary_record(BOB, "0.2"),
                    ],
                }),
            ),
        ]);

        let split = parse_split_record(&rec).unwrap();
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].percentage.to_string(), "0.8");
        assert_eq!(split[1].percentage.to_string(), "0.2");
        assert_eq!(split[0].beneficiary.to_string(), ALICE);
        assert_eq!(split[1].beneficiary.to_string(), BOB);
    }

    #[test]
    fn parse_split_record_rejects_missing_list() {
        let rec = record(vec![field("governanceParty", party(GOV))]);
        assert!(parse_split_record(&rec).is_err());
    }

    #[test]
    fn split_matches_is_order_insensitive_and_exact() {
        let cfg = vec![rb("a", "0.8"), rb("b", "0.2")];
        // reordered -> still a match
        assert!(split_matches(&[rb("b", "0.2"), rb("a", "0.8")], &cfg));
        // wrong percentage -> no match
        assert!(!split_matches(&[rb("a", "0.7"), rb("b", "0.3")], &cfg));
        // off by 1e-10 -> reject (exact Decimal equality, no tolerance)
        assert!(!split_matches(
            &[rb("a", "0.8000000001"), rb("b", "0.1999999999")],
            &cfg
        ));
        // wrong set (different length) -> no match
        assert!(!split_matches(&[rb("a", "1.0")], &cfg));
        // wrong party -> no match
        assert!(!split_matches(&[rb("a", "0.8"), rb("c", "0.2")], &cfg));
    }

    #[test]
    fn is_confirmable_is_default_deny() {
        let cfg = vec![rb("a", "1.0")];
        let good = PendingAssign {
            proposal_cid: "p".into(),
            governance_party: CantonId::parse(GOV).expect("valid canton id"),
            primary_coupon: "c1".into(),
            additional_coupons: vec![],
            new_beneficiaries: vec![rb("a", "1.0")],
        };
        // valid: enrolled label + matching split + coupons unassigned
        assert!(is_confirmable(
            "AssignRewardBeneficiaries",
            &good,
            &cfg,
            true
        ));
        // coupon now assigned -> refuse
        assert!(!is_confirmable(
            "AssignRewardBeneficiaries",
            &good,
            &cfg,
            false
        ));
        // non-enrolled action label -> refuse (default-deny)
        assert!(!is_confirmable("SetRewardSplit", &good, &cfg, true));
        // split mismatch -> refuse
        let bad = PendingAssign {
            new_beneficiaries: vec![rb("z", "1.0")],
            ..good.clone()
        };
        assert!(!is_confirmable(
            "AssignRewardBeneficiaries",
            &bad,
            &cfg,
            true
        ));
    }

    #[test]
    fn parse_assign_record_reads_coupons_and_split() {
        let rec = record(vec![
            field("governanceParty", party(GOV)),
            field("proposer", party(ALICE)),
            field("primaryCoupon", contract_id("c1")),
            field(
                "additionalCoupons",
                value::Sum::List(List {
                    elements: vec![value(contract_id("c2"))],
                }),
            ),
            field(
                "newBeneficiaries",
                value::Sum::List(List {
                    elements: vec![
                        beneficiary_record(ALICE, "0.8"),
                        beneficiary_record(BOB, "0.2"),
                    ],
                }),
            ),
        ]);

        let pa = parse_assign_record("p1", &rec).unwrap();
        assert_eq!(pa.proposal_cid, "p1");
        assert_eq!(pa.governance_party.to_string(), GOV);
        assert_eq!(pa.primary_coupon, "c1");
        assert_eq!(pa.additional_coupons, vec!["c2".to_string()]);
        assert_eq!(pa.new_beneficiaries.len(), 2);
    }

    // ---- proposer (select_batch) --------------------------------------------

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339")
            .with_timezone(&Utc)
    }

    fn coupon(id: &str, expires: &str) -> CouponInfo {
        CouponInfo {
            cid: id.to_string(),
            provider: CantonId::parse(ALICE).expect("valid canton id"),
            amount: "1".parse().expect("valid decimal"),
            expires_at: dt(expires),
        }
    }

    #[test]
    fn select_batch_respects_watermark_margin_and_cap() {
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![
            // ~35h to expiry -> age ~1h < 6h watermark -> too fresh, excluded.
            coupon("young", "2026-07-21T23:00:00Z"),
            // 8h to expiry -> age 28h past watermark, margin ok -> included.
            coupon("ripe", "2026-07-20T20:00:00Z"),
            // 30m to expiry -> inside 2h minting margin -> excluded.
            coupon("urgent", "2026-07-20T12:30:00Z"),
        ];
        let got = select_batch(
            &coupons,
            now,
            chrono::Duration::hours(6),
            chrono::Duration::hours(2),
            100,
        );
        assert_eq!(got, vec!["ripe".to_string()]);
    }

    #[test]
    fn select_batch_caps_size() {
        let now = dt("2026-07-20T12:00:00Z");
        let coupons: Vec<CouponInfo> = (0..10)
            .map(|i| coupon(&format!("c{i}"), "2026-07-20T20:00:00Z"))
            .collect();
        assert_eq!(
            select_batch(
                &coupons,
                now,
                chrono::Duration::hours(6),
                chrono::Duration::hours(2),
                3,
            )
            .len(),
            3
        );
    }

    // ---- confirmer (already_confirmed_by) -----------------------------------

    fn gov_conf(p: &str) -> GovernanceConfirmation {
        GovernanceConfirmation {
            contract_id: "conf".to_string(),
            action: ActionType::GovernanceSetThreshold { new_threshold: 0 },
            confirming_party: CantonId::parse(p).expect("valid canton id"),
            created_at: 0,
            expires_at: 0,
        }
    }

    #[test]
    fn already_confirmed_by_detects_this_member() {
        let action = DomainGovernanceAction {
            proposal_cid: "p".to_string(),
            action_label: "AssignRewardBeneficiaries".to_string(),
            description: None,
            confirmations: vec![gov_conf(ALICE)],
            confirmation_count: 1,
            can_execute: false,
            orphaned: false,
            transfer_details: None,
            accept_transfer_details: None,
            service_request_details: None,
        };
        assert!(already_confirmed_by(
            &action,
            &CantonId::parse(ALICE).expect("valid canton id")
        ));
        assert!(!already_confirmed_by(
            &action,
            &CantonId::parse(BOB).expect("valid canton id")
        ));
    }
}
