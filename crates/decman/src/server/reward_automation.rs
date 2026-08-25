// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! CIP-104 Mode A reward-assignment automation (delegation model).
//!
//! A decparty earns traffic-based app rewards as `RewardCouponV2` coupons that
//! carry `beneficiary = None` and expire unclaimed unless automation
//! reassigns them. Under the delegation model, a single threshold governance
//! vote sets the split and the authorized assigners once, into a
//! `CouponReassignmentDelegation`; from then on, any one listed assigner
//! executes each periodic reassignment directly — no per-round voting. This
//! module holds that automation:
//!
//! * [`active_created_records`] — the one shared **decoded** ACS read. Unlike
//!   `queries::query_contracts_by_template` (cid + base64 blob, no fields) and
//!   `queries::get_contracts` (metadata only), this issues a direct
//!   `StateServiceClient` `GetActiveContracts` — modeled on
//!   `queries::fetch_proposal_infos` — and returns the decoded create-arguments
//!   `Record` (template reads) or the decoded interface-view `Record`
//!   (interface reads).
//! * [`active_delegation`] — reads the decparty's active
//!   `CouponReassignmentDelegation` (its cid, authorized `assigners`, and how
//!   many beneficiaries the split names, to size a chunk); the split's contents
//!   are not read here — `Delegation_Assign` enforces them in DAML.
//! * [`unassigned_coupons`] — reads the decparty's unassigned `RewardCoupon`
//!   interface views.
//! * [`run_reassign_once`] — one reassign sweep: assigns every assignable
//!   unassigned coupon via successive chunked `Delegation_Assign` transactions.
//!
//! The pure record decoders, selection and chunk sizing ([`select_assignable`],
//! [`chunk_size`]) are unit-tested here; the gRPC reads and command submission
//! are exercised by the localnet and devnet integration tests.

use anyhow::{Context, anyhow};
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, Commands, ExerciseCommand, GetActiveContractsRequest, GetLedgerEndRequest, Identifier,
    Record, SubmitAndWaitRequest, Value, command, command_service_client::CommandServiceClient,
    get_active_contracts_response::ContractEntry, value,
};
use chrono::{DateTime, Utc};
use prometheus::{GaugeVec, IntCounter, IntCounterVec};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    utils,
};

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use super::AppState;
use super::action_serializer::{
    field, make_contract_id, make_extra_args, make_list, make_party, make_text_map,
};
use super::event_filters::{
    interface_filter, party_event_format, template_filter, wildcard_filter,
};
use super::handlers::{get_party_credentials, packages};
use super::queries::resolve_contract_package_ref;
use super::record::{
    field_decimal, field_list_len, field_optional_is_none, field_party_id, field_party_list,
    field_time,
};

// ============================================================================
// Shared decoded ACS read
// ============================================================================

/// A Daml module path, e.g. `Splice.Amulet`.
pub(crate) struct Module<'a>(pub &'a str);

/// A Daml template or interface name, e.g. `RewardCouponV2`.
pub(crate) struct Entity<'a>(pub &'a str);

/// What an [`active_created_records`] read selects.
///
/// One value rather than four positional arguments, and the two same-typed
/// halves are wrapped: `package_id`, module and entity were all `&str`, so a
/// transposition compiled, passed clippy, and yielded a filter matching
/// nothing — `Ok(vec![])`, which a caller reads as "the party holds no such
/// contracts". [`Module`] and [`Entity`] make each of those swaps a type
/// error instead, so the hazard is closed rather than narrowed.
pub(crate) struct ContractFilter<'a> {
    pub package_id: &'a str,
    pub module: &'a str,
    pub entity: &'a str,
    /// Read the interface view instead of the concrete template's arguments.
    pub interface_view: bool,
    /// Interface reads only: keep a contract only if this concrete template
    /// created it.
    pub implementer: Option<(&'a str, &'a str)>,
}

impl<'a> ContractFilter<'a> {
    /// A concrete-template read.
    pub fn template(package_id: &'a str, module: Module<'a>, entity: Entity<'a>) -> Self {
        Self {
            package_id,
            module: module.0,
            entity: entity.0,
            interface_view: false,
            implementer: None,
        }
    }

    /// An interface read, admitting every implementation until narrowed by
    /// [`Self::implemented_by`].
    pub fn interface(package_id: &'a str, module: Module<'a>, entity: Entity<'a>) -> Self {
        Self {
            package_id,
            module: module.0,
            entity: entity.0,
            interface_view: true,
            implementer: None,
        }
    }

    /// Keep only contracts created by this concrete template.
    pub fn implemented_by(mut self, module: Module<'a>, entity: Entity<'a>) -> Self {
        self.implementer = Some((module.0, entity.0));
        self
    }
}

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
/// Each entry is `(contract_id, created-event offset, Record)`. The offset
/// orders contracts by creation, which [`active_delegation`] uses to pick the
/// newest of several.
///
/// `implementer` applies to interface reads only: `Some((module, entity))`
/// keeps a contract only if that concrete template created it. An interface
/// read otherwise admits **every** implementation, which is wrong whenever the
/// consumer needs a specific one — see [`unassigned_coupons`].
///
/// Modeled on `queries::fetch_proposal_infos`.
pub(crate) async fn active_created_records(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    filter: ContractFilter<'_>,
) -> anyhow::Result<Vec<(String, i64, Record)>> {
    let ContractFilter {
        package_id,
        module,
        entity,
        interface_view,
        implementer,
    } = filter;
    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let identifier = || Identifier {
        package_id: package_id.to_string(),
        module_name: module.to_string(),
        entity_name: entity.to_string(),
    };
    let filter = if interface_view {
        interface_filter(identifier(), false)
    } else if test_mode {
        wildcard_filter(false)
    } else {
        template_filter(identifier(), false)
    };

    let acs_request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(party_event_format(party_id, vec![filter], true)),
        stream_continuation_token: None,
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
                // An interface read admits every implementation. Keep only the
                // one the caller named, so the reader cannot hand downstream a
                // contract the exercise will refuse.
                if let Some((impl_module, impl_entity)) = implementer
                    && !created.template_id.as_ref().is_some_and(|t| {
                        t.module_name == impl_module && t.entity_name == impl_entity
                    })
                {
                    continue;
                }
                let Some(view) = created.interface_views.iter().find(|v| {
                    v.interface_id
                        .as_ref()
                        .is_some_and(|id| id.module_name == module && id.entity_name == entity)
                }) else {
                    continue;
                };
                if let Some(rec) = view.view_value.clone() {
                    out.push((created.contract_id.clone(), created.offset, rec));
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
                    out.push((created.contract_id.clone(), created.offset, rec));
                }
            }
        }
    }

    Ok(out)
}

// ============================================================================
// CouponReassignmentDelegation (the delegation-model enablement signal)
// ============================================================================

/// The active `CouponReassignmentDelegation` for a decparty: its cid, the set
/// of members authorized to execute `Delegation_Assign`, and how many
/// beneficiaries its split names. The split's *contents* are **not** carried
/// here — they live in the on-ledger contract and `Delegation_Assign` enforces
/// them by construction, so the Rust side never needs to read them.
/// Only the count is read, to size a chunk: one assign creates
/// `coupons × beneficiaries` contracts (see [`chunk_size`]).
pub(crate) struct ActiveDelegation {
    pub cid: String,
    /// The DSO whose coupons this delegation may assign. A coupon from any
    /// other DSO must never enter a batch: as the batch's primary it makes
    /// splice reject every genuine coupon alongside it.
    pub dso: CantonId,
    pub assigners: Vec<CantonId>,
    pub beneficiary_count: usize,
}

/// Decode a `CouponReassignmentDelegation` create-arguments `Record` into an
/// [`ActiveDelegation`]. Reads only `assigners` — the `split` field is
/// intentionally **not** parsed here; `Delegation_Assign` enforces it in DAML.
fn parse_delegation_record(cid: &str, rec: &Record) -> anyhow::Result<ActiveDelegation> {
    let beneficiary_count = field_list_len(rec, "split")?;
    if beneficiary_count == 0 {
        // DAML rejects an empty split at create, so this means a decode problem
        // rather than a real contract. Refuse rather than size a chunk from it.
        return Err(anyhow!("field `split`: delegation names no beneficiaries"));
    }
    Ok(ActiveDelegation {
        cid: cid.to_string(),
        dso: field_party_id(rec, "dso")?,
        assigners: field_party_list(rec, "assigners")?,
        beneficiary_count,
    })
}

/// The active `CouponReassignmentDelegation` for a decparty, read from the
/// ledger, or `None` when there is none (automation not enabled for that
/// decparty). This is the reassign loop's enablement + assigners source.
///
/// A decparty is meant to have at most one. Canton cannot enforce that without
/// a contract key, and it has no cross-participant key uniqueness, so the
/// convention rests on the propose-time 409 guard. If more than
/// one is live anyway, take the newest by created-event offset — the one the
/// most recent vote produced — and warn. Every node reads the same ledger, so
/// they all pick the same contract.
pub(crate) async fn active_delegation(
    config: &NodeConfig,
    packages: &PackageConfig,
    test_mode: bool,
    decparty: &CantonId,
    token: &str,
) -> anyhow::Result<Option<ActiveDelegation>> {
    let Some(package_id) = packages.governance_rewards.as_deref() else {
        return Ok(None);
    };

    let records = active_created_records(
        config,
        decparty,
        Some(token.to_string()),
        test_mode,
        ContractFilter::template(
            package_id,
            Module("Governance.Rewards.CouponReassignmentDelegation"),
            Entity("CouponReassignmentDelegation"),
        ),
    )
    .await?;

    let Some((cid, rec)) = newest_delegation_for(records, decparty) else {
        return Ok(None);
    };
    Ok(Some(parse_delegation_record(&cid, &rec)?))
}

/// Every active `CouponReassignmentDelegation` for `decparty` in the configured
/// package, newest first. The first entry is what [`active_delegation`] returns,
/// so a caller can name the one the automation acts on.
///
/// Only the configured package: a delegation in a superseded package is not
/// exerciseable by the actions this build proposes, so offering one would waste
/// a governance round.
///
/// Strict on purpose: one undecodable delegation fails the whole read, where
/// [`active_delegation`] only ever decodes the newest and so would not notice an
/// older bad one. A delegation nobody can decode is not something to prefill a
/// vote from, and the caller reports the parse error rather than an empty list.
pub(crate) async fn active_delegations(
    config: &NodeConfig,
    packages: &PackageConfig,
    test_mode: bool,
    decparty: &CantonId,
    token: &str,
) -> anyhow::Result<Vec<ActiveDelegation>> {
    let Some(package_id) = packages.governance_rewards.as_deref() else {
        return Ok(Vec::new());
    };

    let records = active_created_records(
        config,
        decparty,
        Some(token.to_string()),
        test_mode,
        ContractFilter::template(
            package_id,
            Module("Governance.Rewards.CouponReassignmentDelegation"),
            Entity("CouponReassignmentDelegation"),
        ),
    )
    .await?;

    delegations_newest_first(records, decparty)
        .iter()
        .map(|(cid, rec)| parse_delegation_record(cid, rec))
        .collect()
}

/// Pick the delegation a decparty should act on (pure): of those naming
/// `decparty`, the one created last.
///
/// A wildcard test-mode read returns every template, and a superseded package
/// version may still be live, so the `decparty` filter is what makes the result
/// this decparty's own. Equal offsets — impossible for two creates on one
/// participant, but cheap to pin — resolve to whichever the read returned first,
/// because [`delegations_newest_first`] sorts stably.
fn newest_delegation_for(
    records: Vec<(String, i64, Record)>,
    decparty: &CantonId,
) -> Option<(String, Record)> {
    delegations_newest_first(records, decparty)
        .into_iter()
        .next()
}

/// Every active delegation naming `decparty` (pure), newest first.
///
/// **The order is a contract**: the first entry is the one the automation acts
/// on, so the vote forms can name it. Canton cannot enforce the per-decparty
/// singleton on a keyless template — the propose-time guard is best-effort, and
/// a racing proposal or a direct ledger submit can leave two active — so the
/// forms list what is really there rather than assuming one.
fn delegations_newest_first(
    records: Vec<(String, i64, Record)>,
    decparty: &CantonId,
) -> Vec<(String, Record)> {
    let mut mine: Vec<(String, i64, Record)> = records
        .into_iter()
        .filter(|(_, _, rec)| field_party_id(rec, "decparty").ok().as_ref() == Some(decparty))
        .collect();

    if mine.len() > 1 {
        tracing::warn!(
            %decparty,
            count = mine.len(),
            "several active CouponReassignmentDelegations; using the newest"
        );
    }
    // Descending offset. `sort_by_key` is stable, so equal offsets — impossible
    // for two creates on one participant, but cheap to pin — keep read order.
    mine.sort_by_key(|(_, offset, _)| std::cmp::Reverse(*offset));
    mine.into_iter().map(|(cid, _, rec)| (cid, rec)).collect()
}

// ============================================================================
// Unassigned reward coupons
// ============================================================================

/// A decparty's unassigned reward coupon (`RewardCoupon` interface view).
///
/// Only what selection needs: `expires_at` orders the batch, `cid` names the
/// coupon to assign.
pub(crate) struct CouponInfo {
    pub cid: String,
    pub expires_at: DateTime<Utc>,
}

/// Read every reward coupon for `decparty` that is still unassigned
/// (`provider == decparty`, `beneficiary` is `None`, and `dso` is the
/// delegation's DSO — see [`parse_unassigned_coupon`]).
///
/// Reads the `RewardCoupon` interface view
/// (`#splice-api-reward-assignment-v1`) but admits **only** contracts created
/// by `Splice.Amulet:RewardCouponV2`.
///
/// That restriction is load-bearing, not tidiness. `Delegation_Assign` fetches
/// the primary as the concrete `RewardCouponV2`, so any other implementation of
/// the interface would pass this reader and fail the exercise. Sorted
/// most-urgent-first it would head the batch on every sweep and stall the drain
/// (see [`drain_assignable`]). The reader admitting exactly what the DAML
/// accepts is what keeps the two in step; a second implementation shipping, or
/// a package skew, must not wedge the engine.
pub(crate) async fn unassigned_coupons(
    config: &NodeConfig,
    decparty: &CantonId,
    dso: &CantonId,
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
        ContractFilter::interface(
            "#splice-api-reward-assignment-v1",
            Module("Splice.Api.RewardAssignmentV1"),
            Entity("RewardCoupon"),
        )
        .implemented_by(Module("Splice.Amulet"), Entity("RewardCouponV2")),
    )
    .await?;

    records
        .iter()
        .filter_map(|(cid, _, rec)| parse_unassigned_coupon(cid, rec, decparty, dso).transpose())
        .collect()
}

/// Decode one `RewardCoupon` interface-view record into a [`CouponInfo`], or
/// `None` when it is not an unassigned coupon for `decparty`. Fail-safe: a
/// coupon whose `provider` differs, or whose `beneficiary` is set (or cannot be
/// confirmed absent), is skipped — never assigned against. Returns `Err` only
/// when a coupon that *does* match cannot be decoded (bad amount/expiry).
fn parse_unassigned_coupon(
    cid: &str,
    rec: &Record,
    decparty: &CantonId,
    dso: &CantonId,
) -> anyhow::Result<Option<CouponInfo>> {
    let provider = field_party_id(rec, "provider")?;
    if &provider != decparty {
        return Ok(None);
    }
    if field_party_id(rec, "dso")? != *dso {
        return Ok(None);
    }
    if !field_optional_is_none(rec, "beneficiary") {
        return Ok(None);
    }
    // Decoded and discarded on purpose: selection does not need the amount, but
    // a coupon that matches and does not decode is a read we do not understand,
    // and erroring beats assigning against it.
    field_decimal(rec, "amount")?;
    Ok(Some(CouponInfo {
        cid: cid.to_string(),
        expires_at: field_time(rec, "expiresAt")?,
    }))
}

// ============================================================================
// Metrics
// ============================================================================

// The reward automation's health signal.

static HEARTBEAT: LazyLock<IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(
        "decman_reward_heartbeat_total",
        "Heartbeats of the reward automation loop, one per wake while it lives — every 60s, or every sweep interval when that is shorter."
    )
    .expect("metric name is a unique literal")
});

static SWEEPS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_sweep_total",
        "Reward sweeps started for a decparty.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static ASSIGNED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_coupons_assigned_total",
        "Coupons successfully reassigned by this node.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static SKIPPED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_coupons_skipped_total",
        "Coupons the ledger rejected individually, to be retried next sweep.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static ZERO_ASSIGNED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_sweep_zero_assigned_total",
        "Reward sweeps that found unassigned coupons and assigned none of them.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static SWEEP_FAILED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_sweep_failed_total",
        "Reward sweeps that ended in an error.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static EXPIRY_READS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_expiry_read_total",
        "Backlog reads that refreshed the expiry gauge without assigning anything.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static AUTH_UNREGISTERED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_auth_unregistered_total",
        "Passes that found no auth registry entry for a decparty this node holds credentials for.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

static OLDEST_EXPIRES_IN: LazyLock<GaugeVec> = LazyLock::new(|| {
    prometheus::register_gauge_vec!(
        "decman_reward_oldest_unassigned_expires_in_seconds",
        "Seconds until the expiry of the oldest coupon this node saw unassigned at its last read.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

/// Registers every family at startup, so a family exists before its first event.
pub(crate) fn register_metrics() {
    LazyLock::force(&HEARTBEAT);
    LazyLock::force(&SWEEPS);
    LazyLock::force(&ASSIGNED);
    LazyLock::force(&SKIPPED);
    LazyLock::force(&ZERO_ASSIGNED);
    LazyLock::force(&SWEEP_FAILED);
    LazyLock::force(&EXPIRY_READS);
    LazyLock::force(&AUTH_UNREGISTERED);
    LazyLock::force(&OLDEST_EXPIRES_IN);
}

/// Exports the per-decparty counters at zero for a decparty this node sweeps. A
/// counter that only appears on its first event leaves a dashboard unable to tell
/// a node that never failed from an instrument that is missing.
fn ensure_outcome_series(label: &str) {
    for counter in [
        &*ASSIGNED,
        &*SKIPPED,
        &*ZERO_ASSIGNED,
        &*SWEEP_FAILED,
        &*EXPIRY_READS,
        &*AUTH_UNREGISTERED,
    ] {
        counter.with_label_values(&[label]).inc_by(0);
    }
}

// ============================================================================
// Coupon batch selection
// ============================================================================

/// A coupon is assignable (pure) iff more than `expiry_margin` remains before
/// it expires.
///
/// The margin exists to keep a coupon that is about to vanish out of a chunk:
/// `Delegation_Assign` is all-or-nothing, so a coupon expiring between the ACS
/// read and the commit fails the whole chunk. It is deliberately **not** a
/// reserve of minting time for the beneficiary — withholding a coupon
/// guarantees nobody ever mints it, whereas assigning it late still lets the
/// beneficiary try, and the per-beneficiary coupons inherit `expiresAt`. For
/// the same reason there is no minimum-age gate: assignment neither consumes
/// the coupon nor shortens it, so assigning as early as we see it leaves the
/// beneficiary the most time to mint.
fn is_assignable(c: &CouponInfo, now: DateTime<Utc>, expiry_margin: chrono::Duration) -> bool {
    c.expires_at - now >= expiry_margin
}

/// Every assignable coupon (pure), ordered most-urgent-first (ascending
/// `expiresAt`) so that under any partial failure the coupons closest to expiry
/// are assigned first. The whole set is returned — it is *not* truncated to a
/// batch size, because a sweep assigns all of it in successive chunked
/// transactions (see [`chunk_size`] and [`run_reassign_once`]); the chunk size
/// bounds one transaction, not a sweep's work.
pub(crate) fn select_assignable(
    coupons: &[CouponInfo],
    now: DateTime<Utc>,
    expiry_margin: chrono::Duration,
) -> Vec<String> {
    let mut selected: Vec<&CouponInfo> = coupons
        .iter()
        .filter(|c| is_assignable(c, now, expiry_margin))
        .collect();
    selected.sort_by_key(|c| c.expires_at);
    selected.into_iter().map(|c| c.cid.clone()).collect()
}

/// How many coupons one `Delegation_Assign` may carry (pure).
///
/// The binding constraint is transaction size, and one assign creates
/// `coupons × beneficiaries` contracts — so the cap is expressed in *output
/// creates* and the coupon count is derived from the delegation's beneficiary
/// count. A fixed coupon count would be `beneficiary_count`-times looser for a
/// wide split than a narrow one. Never returns 0, so a sweep always makes
/// progress even with an implausibly wide split.
pub(crate) fn chunk_size(max_creates: usize, beneficiary_count: usize) -> usize {
    (max_creates / beneficiary_count.max(1)).max(1)
}

// ============================================================================
// Delegation-model per-round assigner (`Delegation_Assign`)
// ============================================================================

/// Build the `Delegation_Assign` choice argument (pure): fields
/// `assigner, primaryCoupon, additionalCoupons, extraArgs`, in that order.
///
/// `extraArgs` is passed through to `RewardCoupon_AssignBeneficiaries`. splice
/// ignores it for `RewardCouponV2`, so an empty one is correct today; the
/// choice takes it so a later coupon version needing a context does not force
/// another DAML change.
fn build_delegation_assign_arg(
    assigner: &CantonId,
    primary: &str,
    additional: &[String],
) -> Record {
    Record {
        record_id: None,
        fields: vec![
            field("assigner", make_party(assigner)),
            field("primaryCoupon", make_contract_id(primary)),
            field(
                "additionalCoupons",
                make_list(additional.iter().map(|c| make_contract_id(c)).collect()),
            ),
            field("extraArgs", make_extra_args(make_text_map(vec![]))),
        ],
    }
}

/// Build the `Identifier` for the `CouponReassignmentDelegation` template
/// (pure). The DAML template lives in module `Governance.Rewards.
/// CouponReassignmentDelegation` (not the shorter `Governance.Rewards`) — the
/// Ledger API `module_name` must be the full module path or the exercise
/// resolves against the wrong (non-existent) template.
fn delegation_template_id(package_id: String) -> Identifier {
    Identifier {
        package_id,
        module_name: "Governance.Rewards.CouponReassignmentDelegation".to_string(),
        entity_name: "CouponReassignmentDelegation".to_string(),
    }
}

/// Exercise `Delegation_Assign` as a plain ledger command (no governance
/// round). Adapted from `execute_confirm_action`
/// (`handlers/governance.rs:2107`); the differences are the target contract
/// (the delegation cid), the choice (`Delegation_Assign`), the template
/// (`Governance.Rewards.CouponReassignmentDelegation`), and `act_as =
/// [assigner]` / `read_as = [decparty]`, because the two are co-hosted.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn submit_delegation_assign(
    config: &NodeConfig,
    decparty: &CantonId,
    assigner: &CantonId,
    token: &str,
    delegation_cid: &str,
    primary: &str,
    additional: &[String],
    packages: &PackageConfig,
) -> anyhow::Result<()> {
    let choice_argument = Value {
        sum: Some(value::Sum::Record(build_delegation_assign_arg(
            assigner, primary, additional,
        ))),
    };
    let fallback = packages
        .governance_rewards
        .as_deref()
        .context("governance_rewards package not configured")?;
    // The delegation may live under an older package ref — resolve its actual
    // one (same as `execute_confirm_action`, governance.rs:2177-2184).
    let package_id = resolve_contract_package_ref(
        config,
        decparty,
        Some(token.to_string()),
        delegation_cid,
        fallback,
    )
    .await;
    let template_id = delegation_template_id(package_id);
    // `ledger_channel` (not a raw `Channel::from_shared`) so this respects the
    // configured Canton TLS/mTLS settings — the automation's own reads go through
    // it via `create_state_client`, and an assign that bypassed it would be the
    // one path unable to talk to a TLS-enabled ledger.
    let channel = config.ledger_channel().await?;
    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: delegation_cid.to_string(),
            choice: "Delegation_Assign".to_string(),
            choice_argument: Some(choice_argument),
        })),
    };
    // act_as = [assigner], read_as = [decparty]. Remaining fields mirror
    // `execute_confirm_action` (governance.rs:2203-2217).
    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![assigner.to_string()],
        read_as: vec![decparty.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
        taps_max_passes: None,
    };
    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    // A non-ASCII byte in the token makes this parse fail. The automation runs
    // in a detached task whose handle is dropped, so a panic here would kill
    // reward assignment silently while the process keeps serving HTTP.
    let auth = format!("Bearer {token}")
        .parse()
        .context("authorization header is not a valid metadata value")?;
    req.metadata_mut().insert("authorization", auth);
    client.submit_and_wait(req).await?; // an assign needs no created-cid readback
    Ok(())
}

/// The earliest expiry among unassigned coupons still worth saving (pure): every
/// coupon that has not yet expired counts, including ones inside the
/// assignability margin. An already-expired coupon is
/// excluded; the DSO's reaper does not archive it promptly (it can sit active
/// for weeks), so including it would pin the gauge to a corpse instead of the
/// backlog a responder can still act on.
fn oldest_expiry(coupons: &[CouponInfo], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    coupons
        .iter()
        .map(|c| c.expires_at)
        .filter(|&expires_at| expires_at > now)
        .min()
}

/// One reassign sweep for a decparty under the delegation model: read the
/// unassigned coupons and assign **all** the assignable ones, in successive
/// chunked `Delegation_Assign` transactions. Nothing assignable is a no-op.
///
/// A sweep drains the whole set rather than assigning one chunk and waiting for
/// the next sweep, so throughput does not depend on the sweep interval: the
/// interval is a latency/cost knob, not a safety-critical one. The chunk bounds
/// one *transaction* (see [`chunk_size`]).
///
/// A transient failure ends the sweep; a rejected command is isolated to the one
/// coupon at fault and the drain continues (see [`drain_assignable`]).
///
/// The create budget and the expiry margin come from [`NodeConfig`] so they can
/// be tuned against a live ledger without a rebuild.
pub(crate) async fn run_reassign_once(
    config: &NodeConfig,
    decparty: &CantonId,
    assigner: &CantonId,
    token: &str,
    delegation: &ActiveDelegation,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let expiry_margin =
        chrono::Duration::seconds(config.reward_min_expiry_margin_secs.min(i64::MAX as u64) as i64);

    let coupons = unassigned_coupons(
        config,
        decparty,
        &delegation.dso,
        Some(token.to_string()),
        test_mode,
        packages,
    )
    .await?;
    let now = Utc::now();
    let oldest = oldest_expiry(&coupons, now);
    let assignable = select_assignable(&coupons, now, expiry_margin);
    if assignable.is_empty() {
        // `visible` is what the ACS read returned, so it separates "coupons
        // present but none assignable yet" from "nothing visible at all".
        //
        // It does NOT separate a decparty that earned nothing from one whose
        // coupons carry providerIsObserver = false (design §4): such a coupon
        // is absent from the ACS entirely, so both report 0. Telling those
        // apart needs a signal this node does not hold.
        tracing::debug!(
            %decparty,
            visible = coupons.len(),
            "no assignable coupons this tick"
        );
        return Ok(oldest);
    }

    let assigned = drain_assignable(
        &assignable,
        chunk_size(config.reward_max_creates, delegation.beneficiary_count),
        decparty,
        |primary, additional| {
            submit_delegation_assign(
                config,
                decparty,
                assigner,
                token,
                &delegation.cid,
                primary,
                additional,
                packages,
            )
        },
    )
    .await;
    if assigned > 0 {
        tracing::info!(%decparty, %assigner, count = assigned, "reassigned coupon batch");
    }
    Ok(oldest)
}

/// Canton error ids that mean this node's view is stale, or the network
/// faltered — not that the command itself was bad.
///
/// Keyed on the **error id**, never the gRPC status code. Devnet shows benign
/// contention arriving as three different codes: `LOCAL_VERDICT_LOCKED_CONTRACTS`
/// is `ABORTED`, `LOCAL_VERDICT_INACTIVE_CONTRACTS` is `NOT_FOUND`, and
/// `UNKNOWN_CONTRACT_SYNCHRONIZERS` is `FAILED_PRECONDITION`. A code-based rule
/// would read that last one as a bad command and fan out a chunk another node
/// had just assigned correctly. Canton's own retryability flag is no better: it
/// marks those categories non-retryable because *resubmitting* cannot work,
/// while this automation re-reads, which does fix them.
///
/// The timeouts come from the 2026-07-29 devnet outage, where five consecutive
/// sweeps each hit a different one and recovered unattended.
/// `MEDIATOR_SAYS_TX_TIMED_OUT` was observed on devnet on 2026-08-20: the coupon
/// it named assigned cleanly on the very next sweep, so the mediator's missing
/// confirmations were the fault and the coupon was never bad.
const TRANSIENT_ASSIGN_ERROR_IDS: &[&str] = &[
    "LOCAL_VERDICT_LOCKED_CONTRACTS",
    "LOCAL_VERDICT_INACTIVE_CONTRACTS",
    "UNKNOWN_CONTRACT_SYNCHRONIZERS",
    "CONTRACT_NOT_FOUND",
    "LOCAL_VERDICT_TIMEOUT",
    "NOT_SEQUENCED_TIMEOUT",
    "SEQUENCER_BACKPRESSURE",
    "REQUEST_TIME_OUT",
    "MEDIATOR_SAYS_TX_TIMED_OUT",
];

/// The Canton error id a failed submission carries, i.e. the message prefix
/// before `(`. `None` when the error is not a ledger rejection at all.
fn canton_error_id(e: &anyhow::Error) -> Option<String> {
    let status = e.downcast_ref::<tonic::Status>()?;
    let id = status.message().split('(').next()?.trim();
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
        .then(|| id.to_string())
}

/// Whether the sweep failed only because a package it reads by name is absent
/// from this participant.
///
/// Both reads name their package by alias, so the ledger rejects the request
/// before it reads a contract. A node in that state fails identically on every
/// sweep and no operator can act on the line, so the loop logs it at `trace`.
///
/// One id covers both reads, because the delegation read runs first: a missing
/// `governance-rewards-automation-v1` fails there, and a missing
/// `splice-api-reward-assignment-v1` fails the same way on the coupon read.
fn package_absent(e: &anyhow::Error) -> bool {
    canton_error_id(e).as_deref() == Some("PACKAGE_NAMES_NOT_FOUND")
}

/// Whether a fresh read on the next sweep is the cure.
///
/// A non-ledger failure (config, transport, auth) is transient here too: it is
/// not attributable to any one coupon, so isolating one would be meaningless.
fn assign_failure_is_transient(e: &anyhow::Error) -> bool {
    match canton_error_id(e) {
        Some(id) => TRANSIENT_ASSIGN_ERROR_IDS.contains(&id.as_str()),
        None => true,
    }
}

/// Assign `assignable` in `chunk`-sized batches via `submit`, returning how many
/// coupons were assigned. The submit step is injected so the chunking, the
/// failure rule and the fan-out are unit-testable without a ledger.
///
/// **A transient failure ends the drain; a rejected command does not.**
///
/// Contention is the overwhelmingly common failure — on a 3-assigner devnet two
/// nodes lose every round — and there a fresh read is the only cure, so the sweep
/// ends and the coupons keep their full TTL. That is [`TRANSIENT_ASSIGN_ERROR_IDS`].
///
/// Any other rejection is attributable to the batch's contents, so the drain
/// re-submits that chunk's coupons **one at a time**, logs each one the ledger
/// still refuses at ERROR, and carries on with the next chunk. splice fetches
/// and validates every `additionalCoupon`, so the culprit can sit anywhere in
/// the chunk; submitting each coupon alone pays every healthy one without
/// having to locate it first.
///
/// One rejected chunk of `n` costs `1 + n` submissions, and that ceiling holds
/// however many of its coupons the ledger refuses. It suits the causes a
/// rejection actually has, because each one is correlated across coupons rather
/// than isolated to a single coupon: a package skew on `RewardCouponV2`, a split
/// splice refuses, a choice context a later coupon version requires. Most or all
/// of a chunk then fails together, so the ERROR lines name every coupon at fault.
///
/// The skip lasts **one sweep only** — there is no cross-sweep quarantine, so the
/// drain stays stateless and a misclassified failure costs one sweep rather than
/// stranding a healthy coupon until restart. A genuinely un-exerciseable coupon
/// is therefore re-found every sweep, which is the point: it keeps producing an
/// ERROR for alerting while every healthy coupon still gets paid.
async fn drain_assignable<'a, F, Fut>(
    assignable: &'a [String],
    chunk: usize,
    decparty: &CantonId,
    mut submit: F,
) -> usize
where
    F: FnMut(&'a str, &'a [String]) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let size = chunk.max(1);
    let mut assigned = 0;
    let mut skipped = 0;

    // Chunks in most-urgent-first order. A rejected chunk is re-submitted one
    // coupon at a time, so a batch the ledger refuses costs 1+n, not a stall.
    'chunks: for lo in (0..assignable.len()).step_by(size) {
        let hi = (lo + size).min(assignable.len());
        let (primary, additional) = assignable[lo..hi]
            .split_first()
            .expect("ranges are built non-empty");

        match submit(primary, additional).await {
            Ok(()) => {
                assigned += hi - lo;
                continue;
            }
            Err(e) if assign_failure_is_transient(&e) => {
                tracing::warn!(
                    %decparty,
                    error = %e,
                    coupon = %primary,
                    assigned,
                    remaining = assignable.len() - lo,
                    "assign chunk failed; ending the sweep to re-read the ledger"
                );
                break;
            }
            // Already a single coupon: there is nothing to fan out to.
            Err(e) if hi - lo == 1 => {
                skipped += 1;
                tracing::error!(
                    %decparty,
                    error = %e,
                    coupon = %primary,
                    error_id = canton_error_id(&e).unwrap_or_default(),
                    "coupon rejected on its own; skipping it for this sweep"
                );
                continue;
            }
            Err(e) => tracing::warn!(
                %decparty,
                error = %e,
                error_id = canton_error_id(&e).unwrap_or_default(),
                coupons = hi - lo,
                "assign chunk rejected; re-submitting its coupons one at a time"
            ),
        }

        for (i, coupon) in assignable[lo..hi].iter().enumerate() {
            // An empty slice borrowed from `assignable` keeps `submit`'s lifetime.
            let alone = &assignable[lo + i..lo + i];
            match submit(coupon, alone).await {
                Ok(()) => assigned += 1,
                Err(e) if assign_failure_is_transient(&e) => {
                    tracing::warn!(
                        %decparty,
                        error = %e,
                        coupon = %coupon,
                        assigned,
                        remaining = assignable.len() - (lo + i),
                        "assign failed mid fan-out; ending the sweep to re-read the ledger"
                    );
                    break 'chunks;
                }
                Err(e) => {
                    skipped += 1;
                    tracing::error!(
                        %decparty,
                        error = %e,
                        coupon = %coupon,
                        error_id = canton_error_id(&e).unwrap_or_default(),
                        "coupon rejected on its own; skipping it for this sweep"
                    );
                }
            }
        }
    }

    let label = decparty.to_string();
    if assigned > 0 {
        ASSIGNED
            .with_label_values(&[&label])
            .inc_by(assigned as u64);
    }
    if skipped > 0 {
        SKIPPED.with_label_values(&[&label]).inc_by(skipped as u64);
        tracing::error!(
            %decparty,
            skipped,
            assigned,
            "some coupons could not be assigned; they will be retried next sweep"
        );
    }
    if sweep_assigned_nothing(assignable.len(), assigned) {
        ZERO_ASSIGNED.with_label_values(&[&label]).inc();
    }
    assigned
}

// ============================================================================
// Background loop + registration
// ============================================================================

/// The loop's wake interval, independent of the reward cadence.
///
/// The heartbeat and the sweep share this one task, so a sweep that blocks for N
/// minutes costs N heartbeats, and one blocking past the stall rule's 10-minute
/// window pages `decman-reward-automation-stalled` even though nothing crashed.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Per-node background loop. It records a heartbeat every [`HEARTBEAT_INTERVAL`] and
/// sweeps each decparty it holds credentials for every
/// `reward_automation_interval_secs`. Enablement is on-ledger: a decparty with no
/// active delegation is skipped.
///
/// The heartbeat shares this task with the sweep deliberately, so a sweep that
/// hangs stops the beat and becomes visible.
pub(crate) async fn run_reward_automation_loop(data: actix_web::web::Data<AppState>) {
    let interval_secs = data.config.reward_automation_interval_secs;
    if interval_secs == 0 {
        tracing::warn!("reward_automation_interval_secs is 0; using 1s instead");
    }
    let sweep_interval = Duration::from_secs(interval_secs.max(1));
    let read_interval = Duration::from_secs(data.config.reward_expiry_read_interval_secs.max(1));

    // The shortest of the three: a configured interval below HEARTBEAT_INTERVAL must
    // still beat at its own rate, or `is_due` only ever sees HEARTBEAT_INTERVAL-sized
    // steps and a short interval is silently floored to it. Beating faster is
    // safe for the stall alert, whose `increase < 1 over 10m` is a lower bound.
    let mut heartbeat =
        tokio::time::interval(HEARTBEAT_INTERVAL.min(sweep_interval).min(read_interval));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Records the wake time, not the tick's own deadline, so scheduling jitter can
    // stretch an interval by up to one heartbeat — benign against a 36h coupon TTL.
    let mut last_sweep: Option<tokio::time::Instant> = None;
    let mut last_read: Option<tokio::time::Instant> = None;
    // Recomputed into the gauge on every heartbeat.
    let mut oldest: HashMap<CantonId, DateTime<Utc>> = HashMap::new();

    loop {
        heartbeat.tick().await;
        HEARTBEAT.inc();
        report_oldest_expiry(&oldest, Utc::now());

        let now = tokio::time::Instant::now();
        let Some(pass) = due_pass(
            last_sweep.map(|t| now.duration_since(t)),
            sweep_interval,
            last_read.map(|t| now.duration_since(t)),
            read_interval,
        ) else {
            continue;
        };
        if pass == Pass::Sweep {
            last_sweep = Some(now);
        }
        // A sweep reads the ledger too, so it satisfies the read clock.
        last_read = Some(now);

        let parties: Vec<CantonId> = data
            .party_credentials
            .read()
            .await
            .iter()
            .map(|p| p.dec_party_id.clone())
            .collect();

        prune_unserved(&mut oldest, &parties);

        for decparty in parties {
            let label = decparty.to_string();
            // Only a sweep moves the sweep counters. A read that fails is not a
            // sweep that failed, and counting it would fire the sweep-failure
            // alert on a signal-path problem.
            match pass {
                Pass::Sweep => {
                    SWEEPS.with_label_values(&[&label]).inc();
                    ensure_outcome_series(&label);
                }
                Pass::ExpiryRead => {
                    EXPIRY_READS.with_label_values(&[&label]).inc();
                }
            }
            let outcome = match run_once_for_party(&data, &decparty, pass).await {
                Ok(outcome) => outcome,
                Err(e) if package_absent(&e) => {
                    tracing::trace!(
                        %decparty,
                        error = %e,
                        "reward automation is off: this participant holds no rewards DAR"
                    );
                    SweepOutcome::Off
                }
                Err(e) => {
                    if pass == Pass::Sweep {
                        SWEEP_FAILED.with_label_values(&[&label]).inc();
                    }
                    match pass {
                        Pass::Sweep => {
                            tracing::warn!(%decparty, error = %e, "reward automation sweep failed");
                        }
                        Pass::ExpiryRead => {
                            tracing::warn!(%decparty, error = %e, "reward expiry read failed");
                        }
                    }
                    SweepOutcome::Failed
                }
            };
            apply_sweep_outcome(&mut oldest, &decparty, outcome);
        }
    }
}

/// Whether an interval has elapsed (pure). `None` is always due: a fresh pod
/// drains its backlog rather than waiting out an interval.
fn is_due(since_last: Option<Duration>, interval: Duration) -> bool {
    since_last.is_none_or(|elapsed| elapsed >= interval)
}

/// What this node may do for a decparty whose delegation it has just read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// The delegation names this node, so it reads the backlog and assigns it.
    Assign,
    /// The delegation does not name this node. It reads the backlog anyway, so
    /// the expiry gauge keeps reporting, and assigns nothing.
    ReportOnly,
}

/// Whether the delegation lets this node assign (pure).
///
/// A delegation that exists and does not name this node is a governance mistake
/// far more often than an intention, and the coupons expire either way. So the
/// backlog still reaches the gauge, and only the assigning stops.
fn role_for(delegation: &ActiveDelegation, member: &CantonId) -> Role {
    if delegation.assigners.contains(member) {
        Role::Assign
    } else {
        Role::ReportOnly
    }
}

/// What a pass over the served decparties does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    /// Read the backlog and assign every assignable coupon.
    Sweep,
    /// Read the backlog only, to refresh the expiry gauge between sweeps.
    ExpiryRead,
}

/// Which pass this tick owes, if any (pure).
///
/// A sweep wins whenever both are due, because a sweep reads the ledger anyway
/// and running both would pay for the same read twice.
fn due_pass(
    since_sweep: Option<Duration>,
    sweep_interval: Duration,
    since_read: Option<Duration>,
    read_interval: Duration,
) -> Option<Pass> {
    if is_due(since_sweep, sweep_interval) {
        Some(Pass::Sweep)
    } else if is_due(since_read, read_interval) {
        Some(Pass::ExpiryRead)
    } else {
        None
    }
}

/// A sweep that found coupons to assign and assigned none of them (pure).
///
/// Keying on the backlog rather than on the refusal tally is what makes the
/// contention `break` count: that path ends the sweep leaving both tallies at
/// zero, so a refusal-only test reads it as a sweep with nothing to do.
fn sweep_assigned_nothing(assignable: usize, assigned: usize) -> bool {
    assignable > 0 && assigned == 0
}

/// Seconds from `now` until `expires_at` (pure). Negative once the moment has
/// passed, and sub-second precise.
fn seconds_until(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    (expires_at - now).num_milliseconds() as f64 / 1000.0
}

/// Publishes the expiry countdown for every remembered decparty.
fn report_oldest_expiry(oldest: &HashMap<CantonId, DateTime<Utc>>, now: DateTime<Utc>) {
    for (decparty, expires_at) in oldest {
        OLDEST_EXPIRES_IN
            .with_label_values(&[&decparty.to_string()])
            .set(seconds_until(*expires_at, now));
    }
}

/// What one sweep learned about a decparty's unassigned backlog.
enum SweepOutcome {
    /// The oldest unassigned coupon expires at this instant.
    Backlog(DateTime<Utc>),
    /// Nothing unassigned, or the automation is switched off for this decparty.
    Empty,
    /// The sweep errored before it could tell.
    Failed,
    /// Party credentials were not available this sweep — e.g. a Keycloak token
    /// acquisition failure. Distinct from `Empty`: this decparty is still meant
    /// to be served (it holds a row in `party_credentials`), so this is an
    /// error condition rather than the automation being off for it.
    CredentialsUnavailable,
    /// The auth registry holds no entry for this decparty, so this node cannot
    /// read its backlog at all. `AuthRegistry::new` skips a party whose token
    /// manager fails to initialise and leaves its `party_credentials` row in
    /// place, so a bad Keycloak secret lands here on every pass until the
    /// registry is rebuilt. Distinct from `CredentialsUnavailable`, which is one
    /// pass's token failure and cures itself. A decparty this node genuinely
    /// stopped serving loses its `party_credentials` row and `prune_unserved`
    /// drops it, so this outcome is always a fault.
    AuthUnregistered,
    /// This participant holds no rewards DAR, so it cannot host a delegation and
    /// the automation is off for it. Distinct from `Empty`, which asserts that
    /// nothing is left to save and drops the gauge series: this outcome asserts
    /// nothing and changes nothing.
    Off,
}

/// Applies one sweep's outcome to the remembered timestamps. The arms are
/// asymmetrical on purpose: dropping the series stops both expiry rules, so only
/// an outcome that asserts the backlog is empty may do it. Every failure keeps
/// the countdown falling, because a coupon nobody can read still expires.
fn apply_sweep_outcome(
    oldest: &mut HashMap<CantonId, DateTime<Utc>>,
    decparty: &CantonId,
    outcome: SweepOutcome,
) {
    match outcome {
        SweepOutcome::Backlog(expires_at) => {
            oldest.insert(decparty.clone(), expires_at);
        }
        // Only `Empty` asserts nothing is left to save, so only `Empty` may drop
        // the series.
        SweepOutcome::Empty => {
            oldest.remove(decparty);
            let _ = OLDEST_EXPIRES_IN.remove_label_values(&[&decparty.to_string()]);
        }
        SweepOutcome::Failed
        | SweepOutcome::CredentialsUnavailable
        | SweepOutcome::AuthUnregistered
        | SweepOutcome::Off => {}
    }
}

/// Forgets every decparty this node no longer holds credentials for, dropping its
/// gauge series with it. A series left behind would count down forever.
fn prune_unserved(oldest: &mut HashMap<CantonId, DateTime<Utc>>, served: &[CantonId]) {
    oldest.retain(|decparty, _| {
        let still_served = served.contains(decparty);
        if !still_served {
            let _ = OLDEST_EXPIRES_IN.remove_label_values(&[&decparty.to_string()]);
        }
        still_served
    });
}

/// One reassign pass for a single decparty under the delegation model. A no-op
/// outcome (`SweepOutcome::Empty`) unless the decparty has an active
/// `CouponReassignmentDelegation` (the enablement signal) naming this node's
/// member party as an assigner. Credentials being unavailable is a distinct
/// outcome (`SweepOutcome::CredentialsUnavailable`): unlike the two enablement
/// no-ops, it is not this decparty's automation being switched off. A missing
/// registry entry is a third (`SweepOutcome::AuthUnregistered`), and it persists
/// where a token failure cures itself.
async fn run_once_for_party(
    data: &actix_web::web::Data<AppState>,
    decparty: &CantonId,
    pass: Pass,
) -> anyhow::Result<SweepOutcome> {
    let pkgs = packages();
    let (token, member) = match get_party_credentials(data, decparty).await {
        Ok(Some(creds)) => creds,
        // A token fetch that failed is one sweep's problem and the callee logged
        // the cause. The decparty is still served, so the countdown keeps falling.
        Err(_) => {
            tracing::warn!(%decparty, "party credentials unavailable for reward sweep");
            return Ok(SweepOutcome::CredentialsUnavailable);
        }
        // This node holds a `party_credentials` row for the decparty and the auth
        // registry has no entry for it, so every pass lands here until the
        // registry is rebuilt. The coupons keep expiring meanwhile, so the
        // countdown stays and a counter carries the fault: a decparty whose auth
        // failed at boot never read a backlog, so it has no gauge series for a
        // rule to fire on.
        Ok(None) => {
            AUTH_UNREGISTERED
                .with_label_values(&[&decparty.to_string()])
                .inc();
            tracing::warn!(
                %decparty,
                "no auth registered for this decparty; its coupons will expire unassigned"
            );
            return Ok(SweepOutcome::AuthUnregistered);
        }
    };
    // Enablement: an active delegation. None => off (no-op).
    let Some(delegation) =
        active_delegation(&data.config, &pkgs, data.test_mode, decparty, &token).await?
    else {
        return Ok(SweepOutcome::Empty);
    };
    let role = role_for(&delegation, &member);
    if role == Role::ReportOnly {
        tracing::warn!(
            %decparty,
            %member,
            assigners = ?delegation.assigners,
            "the delegation does not name this node as an assigner; reporting the backlog without assigning"
        );
    }
    let expires_at = match (role, pass) {
        (Role::Assign, Pass::Sweep) => {
            run_reassign_once(
                &data.config,
                decparty,
                &member,
                &token,
                &delegation,
                data.test_mode,
                &pkgs,
            )
            .await?
        }
        (Role::Assign, Pass::ExpiryRead) | (Role::ReportOnly, _) => {
            read_oldest_expiry(
                &data.config,
                decparty,
                &delegation.dso,
                &token,
                data.test_mode,
                &pkgs,
            )
            .await?
        }
    };
    Ok(match expires_at {
        Some(t) => SweepOutcome::Backlog(t),
        None => SweepOutcome::Empty,
    })
}

/// The expiry of the oldest coupon this decparty still has time to save, read
/// without assigning anything.
///
/// This is the read half of [`run_reassign_once`]. It exists so the gauge can
/// refresh faster than the sweep cadence: the sweep interval is sized to fill a
/// `Delegation_Assign` chunk, which is a transaction-cost choice, and detection
/// should not inherit it.
async fn read_oldest_expiry(
    config: &NodeConfig,
    decparty: &CantonId,
    dso: &CantonId,
    token: &str,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let coupons = unassigned_coupons(
        config,
        decparty,
        dso,
        Some(token.to_string()),
        test_mode,
        packages,
    )
    .await?;
    Ok(oldest_expiry(&coupons, Utc::now()))
}

#[cfg(test)]
mod tests {
    use super::super::types::RewardBeneficiary;
    use super::*;
    use canton_proto_rs::com::daml::ledger::api::v2::{List, Optional, RecordField, Value};

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

    fn optional_none() -> value::Sum {
        value::Sum::Optional(Box::new(Optional { value: None }))
    }

    fn optional_some_party(p: &str) -> value::Sum {
        value::Sum::Optional(Box::new(Optional {
            value: Some(Box::new(value(party(p)))),
        }))
    }

    fn timestamp(micros: i64) -> value::Sum {
        value::Sum::Timestamp(micros)
    }

    fn beneficiary_record(p: &str, pct: &str) -> Value {
        value(value::Sum::Record(record(vec![
            field("beneficiary", party(p)),
            field("percentage", numeric(pct)),
        ])))
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
    const CAROL: &str =
        "carol::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const DAVE: &str = "dave::1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const EVE: &str = "eve::1220eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    #[test]
    fn parse_delegation_record_reads_assigners_and_split() {
        // List values: `value::Sum::List(List { elements: vec![Value, ..] })` — same as the
        // existing parse_split_record test. `party(..)` returns value::Sum, so wrap each with
        // `value(..)`; `beneficiary_record(..)` already returns a Value. `field(label, sum)`
        // takes a value::Sum, so pass `value::Sum::List(..)` directly.
        let rec = record(vec![
            field("decparty", party(GOV)),
            field("dso", party(GOV)),
            field(
                "assigners",
                value::Sum::List(List {
                    elements: vec![value(party(ALICE)), value(party(BOB))],
                }),
            ),
            field(
                "split",
                value::Sum::List(List {
                    elements: vec![
                        beneficiary_record(ALICE, "0.8"),
                        beneficiary_record(BOB, "0.2"),
                    ],
                }),
            ),
        ]);
        let d = parse_delegation_record("00del", &rec).unwrap();
        assert_eq!(d.cid, "00del");
        assert_eq!(d.assigners.len(), 2);
        // Only the split's *length* is read, to size a chunk; its contents stay
        // DAML-enforced and are never used to build a command.
        assert_eq!(d.beneficiary_count, 2);
        assert_eq!(d.dso.to_string(), GOV);
    }

    #[test]
    fn parse_delegation_record_refuses_an_empty_split() {
        // DAML rejects an empty split at create, so this is a decode problem;
        // accepting it would size chunks off a meaningless beneficiary count.
        let rec = record(vec![
            field("dso", party(GOV)),
            field(
                "assigners",
                value::Sum::List(List {
                    elements: vec![value(party(ALICE))],
                }),
            ),
            field("split", value::Sum::List(List { elements: vec![] })),
        ]);
        assert!(parse_delegation_record("00del", &rec).is_err());
    }

    // ---- delegation selection (newest_delegation_for) -----------------------

    /// A delegation record naming `decparty`, enough for the selection filter.
    fn delegation_of(decparty: &str) -> Record {
        record(vec![
            field("decparty", party(decparty)),
            field("dso", party(GOV)),
            field(
                "assigners",
                value::Sum::List(List {
                    elements: vec![value(party(ALICE))],
                }),
            ),
            field(
                "split",
                value::Sum::List(List {
                    elements: vec![beneficiary_record(ALICE, "1.0")],
                }),
            ),
        ])
    }

    #[test]
    fn delegations_are_listed_newest_first() {
        // The revoke form lists every active delegation and must name the one the
        // automation acts on, so the order is part of the contract: index 0 is
        // whatever `newest_delegation_for` would pick.
        let gov = CantonId::parse(GOV).unwrap();
        let records = vec![
            ("00old".to_string(), 100, delegation_of(GOV)),
            ("00new".to_string(), 900, delegation_of(GOV)),
            ("00theirs".to_string(), 999, delegation_of(BOB)),
            ("00mid".to_string(), 500, delegation_of(GOV)),
        ];
        let cids: Vec<String> = delegations_newest_first(records, &gov)
            .into_iter()
            .map(|(cid, _)| cid)
            .collect();
        assert_eq!(cids, vec!["00new", "00mid", "00old"]);
    }

    #[test]
    fn delegations_newest_first_skips_other_decparties() {
        let gov = CantonId::parse(GOV).unwrap();
        let theirs = vec![("00theirs".to_string(), 999, delegation_of(BOB))];
        assert!(delegations_newest_first(theirs, &gov).is_empty());
        assert!(delegations_newest_first(vec![], &gov).is_empty());
    }

    #[test]
    fn newest_delegation_wins_regardless_of_read_order() {
        // The ACS read order is not creation order, so the newest must be picked
        // by offset. Feed the newest first to catch a take-the-first bug.
        let gov = CantonId::parse(GOV).unwrap();
        let records = vec![
            ("00new".to_string(), 900, delegation_of(GOV)),
            ("00old".to_string(), 100, delegation_of(GOV)),
            ("00mid".to_string(), 500, delegation_of(GOV)),
        ];
        let (cid, _) = newest_delegation_for(records, &gov).expect("one is selected");
        assert_eq!(cid, "00new");
    }

    #[test]
    fn newest_delegation_ignores_another_decpartys_delegation() {
        // A wildcard test-mode read returns every template; a delegation whose
        // `decparty` is someone else must never be acted on, even when it is
        // the newest contract in the response.
        let gov = CantonId::parse(GOV).unwrap();
        let records = vec![
            ("00mine".to_string(), 100, delegation_of(GOV)),
            ("00theirs".to_string(), 999, delegation_of(BOB)),
        ];
        let (cid, _) = newest_delegation_for(records, &gov).expect("one is selected");
        assert_eq!(cid, "00mine");
    }

    #[test]
    fn newest_delegation_is_none_when_no_delegation_is_this_decpartys() {
        let gov = CantonId::parse(GOV).unwrap();
        let records = vec![("00theirs".to_string(), 999, delegation_of(BOB))];
        assert!(newest_delegation_for(records, &gov).is_none());
        assert!(newest_delegation_for(vec![], &gov).is_none());
    }

    // ---- unassigned-coupon fail-safe filter (parse_unassigned_coupon) -------

    #[test]
    fn parse_unassigned_coupon_keeps_only_unassigned_for_decparty() {
        let alice = CantonId::parse(ALICE).unwrap();
        let bob = CantonId::parse(BOB).unwrap();
        let dso = CantonId::parse(GOV).unwrap();

        let unassigned = record(vec![
            field("dso", party(GOV)),
            field("provider", party(ALICE)),
            field("beneficiary", optional_none()),
            field("amount", numeric("100.0")),
            field("expiresAt", timestamp(1_700_000_000_000_000)),
        ]);

        // provider == decparty, beneficiary is None, dso matches -> kept.
        let got = parse_unassigned_coupon("00c1", &unassigned, &alice, &dso).unwrap();
        let coupon = got.expect("unassigned coupon for the decparty is kept");
        assert_eq!(coupon.cid, "00c1");

        // provider != decparty -> skipped (fail-safe).
        assert!(
            parse_unassigned_coupon("00c1", &unassigned, &bob, &dso)
                .unwrap()
                .is_none()
        );

        // beneficiary already set -> skipped (never reassign an assigned coupon).
        let assigned = record(vec![
            field("dso", party(GOV)),
            field("provider", party(ALICE)),
            field("beneficiary", optional_some_party(BOB)),
            field("amount", numeric("100.0")),
            field("expiresAt", timestamp(1_700_000_000_000_000)),
        ]);
        assert!(
            parse_unassigned_coupon("00c2", &assigned, &alice, &dso)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_unassigned_coupon_rejects_a_foreign_dso() {
        // `dso` is RewardCouponV2's only signatory, so any party can mint one
        // naming itself dso and this decparty as provider. Letting it into a
        // batch is a denial of service: sorted most-urgent-first it becomes the
        // primary, splice fetches the genuine coupons with the primary's dso,
        // the chunk is rejected, and the sweep ends having assigned nothing.
        let alice = CantonId::parse(ALICE).unwrap();
        let real_dso = CantonId::parse(GOV).unwrap();

        let planted = record(vec![
            field("dso", party(BOB)), // minted by BOB as its own dso
            field("provider", party(ALICE)),
            field("beneficiary", optional_none()),
            field("amount", numeric("100.0")),
            field("expiresAt", timestamp(1_700_000_000_000_000)),
        ]);
        assert!(
            parse_unassigned_coupon("00bad", &planted, &alice, &real_dso)
                .unwrap()
                .is_none(),
            "a coupon from another dso must never enter the batch"
        );
    }

    // ---- selection + chunk sizing -------------------------------------------

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339")
            .with_timezone(&Utc)
    }

    fn coupon(id: &str, expires: &str) -> CouponInfo {
        CouponInfo {
            cid: id.to_string(),
            expires_at: dt(expires),
        }
    }

    #[test]
    fn select_assignable_keeps_margin_and_orders_most_urgent_first() {
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![
            // ~35h to expiry -> freshly minted, but nothing is gained by
            // holding it back -> included.
            coupon("young", "2026-07-21T23:00:00Z"),
            // 8h to expiry -> included.
            coupon("mid", "2026-07-20T20:00:00Z"),
            // 30s to expiry -> may vanish mid-submission and fail the whole
            // chunk -> excluded.
            coupon("expiring", "2026-07-20T12:00:30Z"),
        ];
        let got = select_assignable(&coupons, now, chrono::Duration::minutes(2));
        assert_eq!(got, vec!["mid".to_string(), "young".to_string()]);
    }

    #[test]
    fn select_assignable_keeps_a_coupon_the_beneficiary_may_not_have_time_to_mint() {
        // The margin guards our own submission, not the beneficiary's minting
        // window: withholding a coupon guarantees nobody mints it, while
        // assigning it late still lets the beneficiary try. A coupon 30 min from
        // expiry is well past any minting comfort but safely assignable.
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![coupon("late", "2026-07-20T12:30:00Z")];
        let got = select_assignable(&coupons, now, chrono::Duration::minutes(2));
        assert_eq!(got, vec!["late".to_string()]);
    }

    #[test]
    fn select_assignable_does_not_truncate() {
        // A sweep drains the whole set in chunks, so selection returns all of it;
        // bounding a transaction is chunk_size's job, not selection's.
        let now = dt("2026-07-20T12:00:00Z");
        let coupons: Vec<CouponInfo> = (0..500)
            .map(|i| coupon(&format!("c{i}"), "2026-07-20T20:00:00Z"))
            .collect();
        assert_eq!(
            select_assignable(&coupons, now, chrono::Duration::hours(2)).len(),
            500
        );
    }

    #[test]
    fn chunk_size_scales_with_beneficiary_count() {
        // The cap is on output creates, so a wider split means fewer coupons per
        // transaction — a fixed coupon count would be 10x looser at 20
        // beneficiaries than at 2.
        assert_eq!(chunk_size(100, 2), 50);
        assert_eq!(chunk_size(100, 20), 5);
        assert_eq!(chunk_size(100, 1), 100);
    }

    // ---- drain loop (drain_assignable) --------------------------------------

    fn cids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("c{i}")).collect()
    }

    fn alice() -> CantonId {
        CantonId::parse(ALICE).expect("valid canton id")
    }

    /// Sizes of the chunks a drain submitted, in order.
    type ChunkSizes = std::rc::Rc<std::cell::RefCell<Vec<usize>>>;

    /// Record each submitted chunk's size, answering with `outcomes` in order
    /// (`true` = Ok). Runs out of outcomes => Ok.
    fn recorder(
        outcomes: Vec<bool>,
    ) -> (
        ChunkSizes,
        impl FnMut(&str, &[String]) -> std::future::Ready<anyhow::Result<()>>,
    ) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let mut i = 0;
        let f = move |_p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            let ok = outcomes.get(i).copied().unwrap_or(true);
            i += 1;
            std::future::ready(if ok {
                Ok(())
            } else {
                Err(anyhow!("submission rejected"))
            })
        };
        (seen, f)
    }

    #[tokio::test]
    async fn drain_assigns_the_whole_set_in_one_pass() {
        // The point of draining: 120 coupons at a chunk of 50 is three
        // transactions in ONE sweep, not one transaction and a wait.
        let all = cids(120);
        let (seen, submit) = recorder(vec![]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 120);
        assert_eq!(*seen.borrow(), vec![50, 50, 20]);
    }

    #[tokio::test]
    async fn drain_ends_the_sweep_on_a_failed_chunk() {
        // A failure means the view is stale, and only a fresh read fixes that.
        // Exactly one attempt: no retry at a smaller size (a smaller chunk is
        // not a newer view) and no skip-and-continue (an assigner that took the
        // first coupon has most likely taken the rest).
        let all = cids(120);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![50], "one attempt, then end the sweep");
    }

    #[tokio::test]
    async fn drain_keeps_the_chunks_it_already_committed() {
        // Ending the sweep must not discard earlier successes: the first chunk is
        // assigned, the second fails, and the remainder waits for the next sweep
        // with its full TTL intact.
        let all = cids(120);
        let (seen, submit) = recorder(vec![true, false]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 50);
        assert_eq!(*seen.borrow(), vec![50, 50]);
    }

    #[tokio::test]
    async fn drain_attempts_once_when_the_set_is_smaller_than_the_chunk() {
        // One assignable coupon against a chunk of 50 is the shape devnet
        // contention actually takes. It must cost a single submission.
        let all = cids(1);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![1]);
    }

    // ---- failure classification + fan-out -----------------------------------

    /// A ledger rejection carrying a real Canton error id, shaped exactly as
    /// the devnet logs show it: `ID(category,hash): message`.
    fn canton_err(id: &str) -> anyhow::Error {
        anyhow::Error::new(tonic::Status::aborted(format!(
            "{id}(2,60893414): Rejected transaction"
        )))
    }

    /// Reject any chunk containing `poison` with `err_id`; accept every other
    /// chunk. Records the size of each attempted chunk.
    fn poisoned(
        poison: &'static str,
        err_id: &'static str,
    ) -> (
        ChunkSizes,
        impl FnMut(&str, &[String]) -> std::future::Ready<anyhow::Result<()>>,
    ) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let f = move |p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            let hit = p == poison || additional.iter().any(|c| c == poison);
            std::future::ready(if hit { Err(canton_err(err_id)) } else { Ok(()) })
        };
        (seen, f)
    }

    /// Reject every submission with `err_id`, recording each attempted size.
    fn all_rejected(
        err_id: &'static str,
    ) -> (
        ChunkSizes,
        impl FnMut(&str, &[String]) -> std::future::Ready<anyhow::Result<()>>,
    ) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let f = move |_p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            std::future::ready(Err(canton_err(err_id)))
        };
        (seen, f)
    }

    /// Expected attempt sizes for one chunk of `n` that fails and fans out.
    fn fanned(n: usize) -> Vec<usize> {
        std::iter::once(n)
            .chain(std::iter::repeat_n(1, n))
            .collect()
    }

    #[tokio::test]
    async fn contention_ends_the_sweep_without_fanning_out() {
        // The common case: two of three assigners lose every devnet round. A
        // fresh read is the only cure, so this must cost ONE submission —
        // fanning out here would burn n failing transactions per sweep.
        for id in [
            "LOCAL_VERDICT_LOCKED_CONTRACTS",
            "LOCAL_VERDICT_INACTIVE_CONTRACTS",
            "UNKNOWN_CONTRACT_SYNCHRONIZERS",
        ] {
            let all = cids(64);
            let (seen, submit) = poisoned("c0", id);
            let assigned = drain_assignable(&all, 64, &alice(), submit).await;
            assert_eq!(assigned, 0, "{id} must not assign");
            assert_eq!(*seen.borrow(), vec![64], "{id} must not fan out");
        }
    }

    #[tokio::test]
    async fn a_rejected_coupon_is_isolated_and_the_rest_still_drain() {
        // The head-of-line stall this fix exists for: one coupon the ledger
        // will never accept must not cost the other 63.
        let all = cids(64);
        let (seen, submit) = poisoned("c0", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 64, &alice(), submit).await;
        assert_eq!(assigned, 63, "every healthy coupon is assigned");
        // One batched attempt, then every coupon in that chunk on its own:
        // 1+n submissions, of which n-1 commit.
        assert_eq!(*seen.borrow(), fanned(64));
    }

    #[tokio::test]
    async fn a_rejected_coupon_is_found_wherever_it_sits() {
        // splice fetches and validates every additionalCoupon, so the culprit
        // need not be the primary. Dropping only the primary would advance one
        // coupon per sweep; submitting each on its own does not care where it sat.
        let all = cids(16);
        let (_, submit) = poisoned("c11", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 16, &alice(), submit).await;
        assert_eq!(assigned, 15);
    }

    #[tokio::test]
    async fn a_rejected_coupon_does_not_stop_later_chunks() {
        // The poison sits in chunk 1 of 3. Chunks 2 and 3 must still go, and
        // they must stay batched — only the rejected chunk fans out.
        let all = cids(30);
        let (seen, submit) = poisoned("c3", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 10, &alice(), submit).await;
        assert_eq!(assigned, 29);
        let mut expected = fanned(10);
        expected.extend([10, 10]);
        assert_eq!(*seen.borrow(), expected);
    }

    #[tokio::test]
    async fn a_rejected_singleton_chunk_is_not_resubmitted() {
        // A chunk of one has nothing to fan out to. Re-submitting it would
        // double the cost of the commonest rejection and change nothing.
        let all = cids(1);
        let (seen, submit) = poisoned("c0", "DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 50, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![1]);
    }

    #[tokio::test]
    async fn a_wholly_bad_batch_costs_one_pass_not_two() {
        // The realistic shape: the cause is correlated (a package skew, a split
        // splice refuses), so every coupon fails. The cost stays 1+n, and every
        // coupon at fault is named.
        let all = cids(8);
        let (seen, submit) = all_rejected("DAML_INTERPRETATION_ERROR");
        let assigned = drain_assignable(&all, 8, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), fanned(8));
    }

    #[tokio::test]
    async fn contention_during_the_fan_out_ends_the_sweep() {
        // Another assigner took the rest of the chunk while we were fanning out.
        // A fresh read is the cure, so stop — and keep what already committed.
        let all = cids(4);
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let submit = move |_p: &str, additional: &[String]| {
            log.borrow_mut().push(additional.len() + 1);
            let calls = log.borrow().len();
            std::future::ready(match calls {
                1 => Err(canton_err("DAML_INTERPRETATION_ERROR")), // the batch
                3 => Err(canton_err("LOCAL_VERDICT_LOCKED_CONTRACTS")),
                _ => Ok(()),
            })
        };
        let assigned = drain_assignable(&all, 4, &alice(), submit).await;
        assert_eq!(assigned, 1, "the singleton that committed is kept");
        assert_eq!(
            *seen.borrow(),
            vec![4, 1, 1],
            "stops at the transient error"
        );
    }

    #[tokio::test]
    async fn an_unrecognized_ledger_rejection_is_isolated_not_swallowed() {
        // An id we have never seen is treated as a bad command, so a coupon
        // that can never be exercised is still contained. The cost of being
        // wrong is one sweep: nothing is quarantined across sweeps.
        let all = cids(8);
        let (_, submit) = poisoned("c0", "SOME_FUTURE_CANTON_ERROR");
        let assigned = drain_assignable(&all, 8, &alice(), submit).await;
        assert_eq!(assigned, 7);
    }

    #[tokio::test]
    async fn a_non_ledger_failure_ends_the_sweep() {
        // A transport or config failure is not attributable to any one coupon,
        // so isolating one would be meaningless.
        let all = cids(8);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 8, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(*seen.borrow(), vec![8], "no fan-out on a non-ledger error");
    }

    #[test]
    fn transient_classification_keys_on_the_error_id_not_the_grpc_code() {
        // All three devnet contention errors arrive under DIFFERENT gRPC codes
        // (ABORTED / NOT_FOUND / FAILED_PRECONDITION), so a code-based rule
        // would misread one of them. Same code here, different ids.
        assert!(assign_failure_is_transient(&canton_err(
            "LOCAL_VERDICT_LOCKED_CONTRACTS"
        )));
        assert!(!assign_failure_is_transient(&canton_err(
            "DAML_INTERPRETATION_ERROR"
        )));
        // FAILED_PRECONDITION carrying benign contention — the case a
        // code-based classifier gets wrong.
        let fp = anyhow::Error::new(tonic::Status::failed_precondition(
            "UNKNOWN_CONTRACT_SYNCHRONIZERS(9,6a504b42): The following contracts have been archived",
        ));
        assert!(assign_failure_is_transient(&fp));
    }

    #[test]
    fn a_mediator_confirmation_timeout_is_transient() {
        // Devnet, 2026-08-20: this arrived as ABORTED and was counted a coupon
        // rejection, so it logged at ERROR and fired the coupon-rejected alert.
        // The same coupon assigned on the next sweep, so nothing was wrong with
        // it. Every other timeout in the list is classified the same way.
        let e = anyhow::Error::new(tonic::Status::aborted(
            "MEDIATOR_SAYS_TX_TIMED_OUT(2,0): Rejected transaction as the mediator did not \
             receive sufficient confirmations within the expected timeframe.",
        ));
        assert_eq!(
            canton_error_id(&e).as_deref(),
            Some("MEDIATOR_SAYS_TX_TIMED_OUT")
        );
        assert!(
            assign_failure_is_transient(&e),
            "a mediator timeout is not attributable to the coupon"
        );
    }

    #[test]
    fn canton_error_id_survives_added_context() {
        // A future `.context(..)` on the submit call must not silently turn
        // every rejection into an unclassifiable one.
        let e = Err::<(), _>(canton_err("LOCAL_VERDICT_LOCKED_CONTRACTS"))
            .context("submitting Delegation_Assign")
            .unwrap_err();
        assert_eq!(
            canton_error_id(&e).as_deref(),
            Some("LOCAL_VERDICT_LOCKED_CONTRACTS")
        );
        assert!(assign_failure_is_transient(&e));
    }

    #[test]
    fn only_an_absent_package_is_quiet() {
        // The devnet message, verbatim. The gRPC code here is deliberately not
        // the one Canton sends: the error id is what classifies.
        let absent = anyhow::Error::new(tonic::Status::internal(
            "PACKAGE_NAMES_NOT_FOUND(11,21a20a9f): The following package names do not match \
             upgradable packages uploaded on this participant: [governance-rewards-automation-v1].",
        ));
        assert!(package_absent(&absent));
        // A context added on the way up must not send the line back to warn.
        assert!(package_absent(
            &Err::<(), _>(absent)
                .context("reading the delegation")
                .unwrap_err()
        ));

        // Another ledger rejection, then a failure carrying no ledger status at
        // all, such as auth or transport.
        assert!(!package_absent(&canton_err("DAML_INTERPRETATION_ERROR")));
        assert!(!package_absent(&anyhow::anyhow!("connection refused")));
    }

    #[test]
    fn chunk_size_never_stalls_a_sweep() {
        // A split wider than the create budget still yields a 1-coupon chunk
        // rather than 0, which would loop forever making no progress.
        assert_eq!(chunk_size(100, 500), 1);
        assert_eq!(chunk_size(0, 2), 1);
        // A malformed 0 count must not divide by zero.
        assert_eq!(chunk_size(100, 0), 100);
    }

    #[test]
    fn build_delegation_assign_arg_shape() {
        // rb(..).beneficiary yields a CantonId (this module has no canton_id helper);
        // rb takes a bare prefix and appends a fixed namespace itself.
        let rec =
            build_delegation_assign_arg(&rb("m1", "1.0").beneficiary, "00c1", &["00c2".into()]);
        let labels: Vec<&str> = rec.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "assigner",
                "primaryCoupon",
                "additionalCoupons",
                "extraArgs"
            ]
        );
    }

    #[test]
    fn delegation_template_id_uses_full_module_path() {
        // The DAML template lives in `Governance.Rewards.CouponReassignmentDelegation`,
        // not the shorter `Governance.Rewards` — a truncated module_name makes the
        // Ledger API resolve the exercise against a non-existent template.
        let id = delegation_template_id("pkg123".to_string());
        assert_eq!(id.package_id, "pkg123");
        assert_eq!(
            id.module_name,
            "Governance.Rewards.CouponReassignmentDelegation"
        );
        assert_eq!(id.entity_name, "CouponReassignmentDelegation");
    }

    fn gathered_family_names() -> Vec<String> {
        prometheus::gather()
            .into_iter()
            .map(|f| f.get_name().to_string())
            .collect()
    }

    #[test]
    fn the_heartbeat_is_exposed_before_any_sweep_runs() {
        // The stall alert watches this family, so it must exist from boot.
        register_metrics();
        assert!(
            gathered_family_names()
                .iter()
                .any(|n| n == "decman_reward_heartbeat_total")
        );
    }

    #[test]
    fn a_labelled_family_appears_once_a_decparty_is_known() {
        // `gather()` omits a labelled family with no series, so these appear only
        // once a decparty is known.
        register_metrics();
        SWEEPS.with_label_values(&["cbtc-network::1220test"]).inc(); // any string is a valid label
        assert!(
            gathered_family_names()
                .iter()
                .any(|n| n == "decman_reward_sweep_total")
        );
    }

    #[test]
    fn the_outcome_counters_read_zero_before_their_first_event() {
        // A dashboard cannot tell a missing family from a node that never failed,
        // so a served decparty exports all six at zero from its first sweep.
        // The read counter needs this most: a node whose sweep interval is
        // shorter than its read interval never increments it at all.
        register_metrics();
        let label = "cbtc-network::1220outcomezero";
        ensure_outcome_series(label);

        let families = prometheus::gather();
        for name in [
            "decman_reward_coupons_assigned_total",
            "decman_reward_coupons_skipped_total",
            "decman_reward_sweep_zero_assigned_total",
            "decman_reward_sweep_failed_total",
            "decman_reward_expiry_read_total",
            "decman_reward_auth_unregistered_total",
        ] {
            let family = families
                .iter()
                .find(|f| f.get_name() == name)
                .unwrap_or_else(|| panic!("{name} is missing from the registry"));
            let metric = family
                .get_metric()
                .iter()
                .find(|m| m.get_label().iter().any(|l| l.get_value() == label))
                .unwrap_or_else(|| panic!("{name} has no series for the decparty"));
            assert_eq!(
                metric.get_counter().get_value(),
                0.0,
                "{name} should read zero"
            );
        }
    }

    #[test]
    fn the_expiry_countdown_falls_with_real_time_not_with_the_sweep() {
        // Falls with real time, not with the sweep cadence.
        let decparty = CantonId::parse(GOV).expect("GOV is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();
        oldest.insert(decparty.clone(), expires_at);

        report_oldest_expiry(&oldest, dt("2026-07-20T12:00:00Z"));
        let six_hours = OLDEST_EXPIRES_IN
            .get_metric_with_label_values(&[&decparty.to_string()])
            .expect("label values are valid")
            .get();
        assert_eq!(six_hours, 6.0 * 3600.0);

        // Twenty minutes later, with no sweep in between.
        report_oldest_expiry(&oldest, dt("2026-07-20T12:20:00Z"));
        let after = OLDEST_EXPIRES_IN
            .get_metric_with_label_values(&[&decparty.to_string()])
            .expect("label values are valid")
            .get();
        assert_eq!(after, 6.0 * 3600.0 - 1200.0);
    }

    #[test]
    fn a_failed_sweep_keeps_the_countdown_running() {
        // A failure must leave the timestamp alone.
        let decparty = CantonId::parse(ALICE).expect("ALICE is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Backlog(expires_at));
        assert_eq!(oldest.get(&decparty), Some(&expires_at), "a backlog writes");

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Failed);
        assert_eq!(
            oldest.get(&decparty),
            Some(&expires_at),
            "a failure must leave the timestamp in place"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Empty);
        assert!(oldest.is_empty(), "an empty backlog removes");
    }

    #[test]
    fn unavailable_credentials_keep_the_countdown_running() {
        // A Keycloak token failure must not read as "safely off": the timestamp
        // must survive exactly as it does on `Failed`.
        // Its own decparty fixture, since these tests share a process-wide
        // metrics registry and run in parallel.
        let decparty = CantonId::parse(CAROL).expect("CAROL is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Backlog(expires_at));
        assert_eq!(oldest.get(&decparty), Some(&expires_at), "a backlog writes");

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::CredentialsUnavailable);
        assert_eq!(
            oldest.get(&decparty),
            Some(&expires_at),
            "unavailable credentials must leave the timestamp in place"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Empty);
        assert!(oldest.is_empty(), "an empty backlog removes");
    }

    #[test]
    fn a_missing_dar_changes_nothing() {
        // A participant with no rewards DAR is a no-op, not a failure and not an
        // empty backlog. `Empty` would drop the gauge series, which asserts that
        // nothing is left to save.
        let decparty = CantonId::parse(EVE).expect("EVE is a valid canton id");
        let expires_at = dt("2026-07-20T18:00:00Z");
        let mut oldest = HashMap::new();

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Backlog(expires_at));
        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Off);
        assert_eq!(
            oldest.get(&decparty),
            Some(&expires_at),
            "a missing DAR must leave the timestamp exactly as it was"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::Empty);
        assert!(oldest.is_empty(), "an empty backlog still removes");
    }

    #[test]
    fn losing_auth_for_a_decparty_keeps_its_countdown() {
        // `AuthRegistry::new` skips a party whose token manager fails to
        // initialise and leaves its `party_credentials` row alone, so a bad
        // Keycloak secret puts a decparty in the loop's party list and out of the
        // registry until a restart. `prune_unserved` cannot catch it, because that
        // keys on the rows, which still exist. The coupons expire meanwhile, so
        // both failures have to keep the countdown falling.
        let decparty = CantonId::parse(BOB).expect("BOB is a valid canton id");
        let mut oldest = HashMap::new();
        oldest.insert(decparty.clone(), dt("2026-07-20T18:00:00Z"));

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::CredentialsUnavailable);
        assert!(
            oldest.contains_key(&decparty),
            "one pass's token failure is transient, so the countdown keeps running"
        );

        apply_sweep_outcome(&mut oldest, &decparty, SweepOutcome::AuthUnregistered);
        assert!(
            oldest.contains_key(&decparty),
            "a missing registry entry does not empty the backlog, so the countdown stays"
        );
    }

    #[test]
    fn a_decparty_this_node_stopped_serving_is_forgotten() {
        // Otherwise the countdown outlives the credentials.
        let decparty = CantonId::parse(BOB).expect("BOB is a valid canton id");
        let mut oldest = HashMap::new();
        oldest.insert(decparty.clone(), dt("2026-07-20T18:00:00Z"));

        prune_unserved(&mut oldest, std::slice::from_ref(&decparty));
        assert!(
            oldest.contains_key(&decparty),
            "still served, so still tracked"
        );

        prune_unserved(&mut oldest, &[]);
        assert!(oldest.is_empty(), "no longer served, so forgotten");
    }

    #[test]
    fn the_first_sweep_of_a_process_is_always_due() {
        assert!(is_due(None, Duration::from_secs(21_600)));
    }

    #[test]
    fn a_sweep_is_due_once_the_interval_has_elapsed() {
        let interval = Duration::from_secs(600);
        assert!(!is_due(Some(Duration::from_secs(599)), interval));
        assert!(is_due(Some(Duration::from_secs(600)), interval));
        assert!(is_due(Some(Duration::from_secs(601)), interval));
    }

    // ---- the two clocks (due_pass) -------------------------------------------

    /// Devnet's cadence: a 6h sweep against the default hourly read.
    const SWEEP: Duration = Duration::from_secs(21_600);
    const READ: Duration = Duration::from_secs(3_600);

    fn secs(n: u64) -> Option<Duration> {
        Some(Duration::from_secs(n))
    }

    // ---- the assigner role (role_for) ---------------------------------------

    fn delegation_naming(assigners: &[&str]) -> ActiveDelegation {
        ActiveDelegation {
            cid: "00cid".to_string(),
            dso: CantonId::parse(GOV).expect("GOV is a valid canton id"),
            assigners: assigners
                .iter()
                .map(|a| CantonId::parse(a).expect("valid canton id"))
                .collect(),
            beneficiary_count: 2,
        }
    }

    #[test]
    fn a_listed_assigner_may_assign() {
        let d = delegation_naming(&[ALICE, BOB]);
        let me = CantonId::parse(ALICE).expect("ALICE is a valid canton id");
        assert_eq!(role_for(&d, &me), Role::Assign);
    }

    #[test]
    fn a_node_the_delegation_does_not_name_still_reports() {
        // A vote naming the wrong party is a mistake, not an off switch, so the
        // backlog must keep reaching the expiry gauge.
        let d = delegation_naming(&[ALICE, BOB]);
        let me = CantonId::parse(CAROL).expect("CAROL is a valid canton id");
        assert_eq!(role_for(&d, &me), Role::ReportOnly);
    }

    #[test]
    fn a_delegation_naming_nobody_reports() {
        let d = delegation_naming(&[]);
        let me = CantonId::parse(ALICE).expect("ALICE is a valid canton id");
        assert_eq!(role_for(&d, &me), Role::ReportOnly);
    }

    #[test]
    fn the_first_tick_sweeps_rather_than_only_reading() {
        assert_eq!(due_pass(None, SWEEP, None, READ), Some(Pass::Sweep));
    }

    #[test]
    fn a_due_sweep_wins_over_a_due_read() {
        // A sweep reads the ledger anyway, so running both would pay for the
        // same read twice.
        assert_eq!(
            due_pass(secs(21_600), SWEEP, secs(3_600), READ),
            Some(Pass::Sweep)
        );
    }

    #[test]
    fn a_read_runs_between_sweeps() {
        assert_eq!(
            due_pass(secs(3_600), SWEEP, secs(3_600), READ),
            Some(Pass::ExpiryRead)
        );
    }

    #[test]
    fn neither_clock_due_runs_nothing() {
        assert_eq!(due_pass(secs(600), SWEEP, secs(600), READ), None);
    }

    #[test]
    fn a_read_interval_longer_than_the_sweep_never_binds() {
        // Mainnet's 300s default against an hourly read: every pass is a sweep,
        // so the read clock costs nothing.
        let sweep = Duration::from_secs(300);
        assert_eq!(
            due_pass(secs(300), sweep, secs(300), READ),
            Some(Pass::Sweep)
        );
        assert_eq!(due_pass(secs(120), sweep, secs(120), READ), None);
    }

    #[test]
    fn seconds_until_counts_down_to_the_expiry() {
        let now = dt("2026-07-20T12:00:00Z");
        assert_eq!(seconds_until(dt("2026-07-20T18:00:00Z"), now), 6.0 * 3600.0);
    }

    #[test]
    fn seconds_until_is_negative_for_an_expired_coupon() {
        // Negative rather than clamped: the value is lost, and by how long matters.
        let now = dt("2026-07-20T12:00:00Z");
        assert_eq!(seconds_until(dt("2026-07-20T11:00:00Z"), now), -3600.0);
    }

    #[test]
    fn seconds_until_keeps_sub_second_precision() {
        // The gauge is compared across samples minutes apart, so a truncated
        // second would read as drift.
        let now = dt("2026-07-20T12:00:00Z");
        assert_eq!(seconds_until(dt("2026-07-20T12:00:00.500Z"), now), 0.5);
    }

    #[test]
    fn oldest_expiry_takes_the_earliest_of_the_pile() {
        // Not the first read, and not the most urgent assignable one.
        let now = dt("2026-07-20T00:00:00Z");
        let coupons = vec![
            coupon("a", "2026-07-20T18:00:00Z"),
            coupon("b", "2026-07-20T12:00:00Z"),
            coupon("c", "2026-07-20T15:00:00Z"),
        ];
        assert_eq!(
            oldest_expiry(&coupons, now),
            Some(dt("2026-07-20T12:00:00Z"))
        );
    }

    #[test]
    fn oldest_expiry_is_none_for_an_empty_backlog() {
        // The loop reads this as "drop the series", so it must not be a zero.
        let now = dt("2026-07-20T00:00:00Z");
        assert_eq!(oldest_expiry(&[], now), None);
    }

    #[test]
    fn oldest_expiry_ignores_an_expired_coupon_in_favour_of_a_later_live_one() {
        // The expired coupon has the numerically smallest `expires_at` in the
        // pile — without the expiry filter it would win the minimum.
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![
            coupon("dead", "2026-07-19T00:00:00Z"),
            coupon("live", "2026-07-20T18:00:00Z"),
        ];
        assert_eq!(
            oldest_expiry(&coupons, now),
            Some(dt("2026-07-20T18:00:00Z"))
        );
    }

    #[test]
    fn oldest_expiry_is_none_when_every_coupon_has_expired() {
        let now = dt("2026-07-20T12:00:00Z");
        let coupons = vec![
            coupon("dead-1", "2026-07-19T00:00:00Z"),
            coupon("dead-2", "2026-07-18T00:00:00Z"),
        ];
        assert_eq!(oldest_expiry(&coupons, now), None);
    }

    #[test]
    fn a_sweep_that_paid_nothing_while_every_coupon_was_refused_counts() {
        assert!(sweep_assigned_nothing(3, 0));
    }

    #[test]
    fn a_sweep_that_ended_before_it_paid_anything_counts() {
        // The contention `break` ends a sweep with both tallies at zero while a
        // backlog is still waiting.
        assert!(sweep_assigned_nothing(9, 0));
    }

    #[test]
    fn a_sweep_that_paid_something_does_not_count() {
        assert!(!sweep_assigned_nothing(10, 7));
    }

    #[test]
    fn a_sweep_with_nothing_to_do_does_not_count() {
        assert!(!sweep_assigned_nothing(0, 0));
    }

    #[tokio::test]
    async fn drain_counts_a_sweep_that_ended_before_assigning_anything() {
        // Its own decparty fixture, since these tests share a process-wide
        // metrics registry and run in parallel.
        let decparty = CantonId::parse(DAVE).expect("DAVE is a valid canton id");
        let label = decparty.to_string();
        let read = || {
            ZERO_ASSIGNED
                .get_metric_with_label_values(&[&label])
                .expect("label values are valid")
                .get()
        };
        let before = read();

        let all = cids(120);
        let (_seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 50, &decparty, submit).await;

        assert_eq!(assigned, 0);
        assert_eq!(read(), before + 1);
    }
}
