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
//! * [`run_reassign_once`] — one reassign tick: assigns every assignable
//!   unassigned coupon via successive chunked `Delegation_Assign` transactions.
//!
//! The pure record decoders, selection and chunk sizing ([`select_assignable`],
//! [`chunk_size`]) are unit-tested here; the gRPC reads and command submission
//! are exercised by the localnet and devnet integration tests.

use std::collections::HashMap;

use anyhow::{Context, anyhow};
use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, Commands, CumulativeFilter, EventFormat, ExerciseCommand, Filters,
    GetActiveContractsRequest, GetLedgerEndRequest, Identifier, InterfaceFilter, Record,
    SubmitAndWaitRequest, TemplateFilter, Value, WildcardFilter, command,
    command_service_client::CommandServiceClient, cumulative_filter,
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
use super::action_serializer::{field, make_contract_id, make_list, make_party};
use super::handlers::{get_party_credentials, packages};
use super::queries::resolve_contract_package_ref;

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

/// Return true iff `label` is an `Optional` field carrying `None`.
///
/// A missing field, or a non-optional value, returns false — the caller then
/// treats the contract as *not* unassigned (fail-safe: never assign against a
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
// CouponReassignmentDelegation (the delegation-model enablement signal)
// ============================================================================

/// The active `CouponReassignmentDelegation` for a decparty: its cid, the set
/// of members authorized to execute `Delegation_Assign`, and how many
/// beneficiaries its split names. The split's *contents* are **not** carried
/// here — they live in the on-ledger contract and `Delegation_Assign` enforces
/// them by construction (spec §12), so the Rust side never needs to read them.
/// Only the count is read, to size a chunk: one assign creates
/// `coupons × beneficiaries` contracts (see [`chunk_size`]).
pub(crate) struct ActiveDelegation {
    pub cid: String,
    pub assigners: Vec<CantonId>,
    pub beneficiary_count: usize,
}

/// Return the number of elements in a `List` field.
fn field_list_len(rec: &Record, label: &str) -> anyhow::Result<usize> {
    match record_field(rec, label) {
        Some(value::Sum::List(l)) => Ok(l.elements.len()),
        _ => Err(anyhow!("field `{label}`: expected a List value")),
    }
}

/// Read a list-of-`Party` field, parsing each element into a [`CantonId`].
/// Mirrors `field_contract_id_list`, decoding each element the same way
/// `field_party_id` decodes a single `Party` value.
fn field_party_list(rec: &Record, label: &str) -> anyhow::Result<Vec<CantonId>> {
    let list = match record_field(rec, label) {
        Some(value::Sum::List(l)) => l,
        _ => return Err(anyhow!("field `{label}`: expected a List value")),
    };
    list.elements
        .iter()
        .map(|elem| match elem.sum.as_ref() {
            Some(value::Sum::Party(p)) => p
                .parse::<CantonId>()
                .with_context(|| format!("field `{label}`: invalid party id `{p}`")),
            _ => Err(anyhow!("field `{label}`: element is not a Party")),
        })
        .collect()
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
        assigners: field_party_list(rec, "assigners")?,
        beneficiary_count,
    })
}

/// The active `CouponReassignmentDelegation` for a decparty, read from the
/// ledger, or `None` when there is none (automation not enabled for that
/// decparty). Defends the keyless-singleton invariant: more than one active
/// delegation for the same decparty is refused rather than guessed at. This
/// is the reassign loop's enablement + assigners source.
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
        package_id,
        "Governance.Rewards.CouponReassignmentDelegation",
        "CouponReassignmentDelegation",
        false,
    )
    .await?;

    // Defend the keyless-singleton invariant: keep only delegations whose
    // `decparty` field is this decparty.
    let mut mine: Vec<(String, Record)> = records
        .into_iter()
        .filter(|(_, rec)| field_party_id(rec, "decparty").ok().as_ref() == Some(decparty))
        .collect();

    match mine.len() {
        0 => Ok(None),
        1 => {
            let (cid, rec) = mine.remove(0);
            Ok(Some(parse_delegation_record(&cid, &rec)?))
        }
        n => {
            tracing::warn!(%decparty, count = n, "ambiguous CouponReassignmentDelegation — refusing");
            Err(anyhow!(
                "ambiguous CouponReassignmentDelegation: {n} active — refusing"
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
    /// Currently unused — the batching logic keys off `cid` + `expires_at`
    /// only. Retained for future operator logging / devnet IT assertions.
    #[allow(dead_code)]
    pub provider: CantonId,
    /// See `provider` — currently unused, retained for the same reason.
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
        if let Some(coupon) = parse_unassigned_coupon(&cid, &rec, decparty)? {
            out.push(coupon);
        }
    }
    Ok(out)
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
) -> anyhow::Result<Option<CouponInfo>> {
    let provider = field_party_id(rec, "provider")?;
    if &provider != decparty {
        return Ok(None);
    }
    if !field_optional_is_none(rec, "beneficiary") {
        return Ok(None);
    }
    Ok(Some(CouponInfo {
        cid: cid.to_string(),
        provider,
        amount: field_decimal(rec, "amount")?,
        expires_at: field_time(rec, "expiresAt")?,
    }))
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
/// batch size, because a tick assigns all of it in successive chunked
/// transactions (see [`chunk_size`] and [`run_reassign_once`]); the chunk size
/// bounds one transaction, not a tick's work.
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
/// wide split than a narrow one. Never returns 0, so a tick always makes
/// progress even with an implausibly wide split.
pub(crate) fn chunk_size(max_creates: usize, beneficiary_count: usize) -> usize {
    (max_creates / beneficiary_count.max(1)).max(1)
}

// ============================================================================
// Delegation-model per-round assigner (`Delegation_Assign`)
// ============================================================================

/// Build the `Delegation_Assign` choice argument (pure): fields
/// `assigner, primaryCoupon, additionalCoupons`, in that order.
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
/// [assigner]` / `read_as = [decparty]` (co-hosting, spec §4.6).
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
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;
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
    };
    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    client.submit_and_wait(req).await?; // an assign needs no created-cid readback
    Ok(())
}

/// One reassign tick for a decparty under the delegation model: read the
/// unassigned coupons and assign **all** the assignable ones, in successive
/// chunked `Delegation_Assign` transactions. Nothing assignable is a no-op.
///
/// A tick drains the whole set rather than assigning one chunk and waiting for
/// the next tick, so throughput does not depend on the tick interval: the
/// interval is a latency/cost knob, not a safety-critical one. The chunk bounds
/// one *transaction* (spec §9/§11; see [`chunk_size`]).
///
/// Failure handling, since the real transaction-size ceiling is unmeasured:
/// a failed chunk is retried at half the size, down to a single coupon, which
/// lets an oversized chunk find a size the ledger accepts. Failures at a single
/// coupon are almost always contention instead — another assigner took the
/// coupon and this node's view is stale — so after
/// `MAX_SINGLE_COUPON_FAILURES` of them the tick stops and lets the next tick
/// re-read the ledger, rather than grinding through a set that is already gone.
///
/// The create budget and the expiry margin come from [`NodeConfig`] so they can
/// be tuned against a live ledger without a rebuild; only the give-up count is
/// fixed, since nothing about a deployment changes the right value for it.
pub(crate) async fn run_reassign_once(
    config: &NodeConfig,
    decparty: &CantonId,
    assigner: &CantonId,
    token: &str,
    delegation: &ActiveDelegation,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<()> {
    const MAX_SINGLE_COUPON_FAILURES: usize = 3;
    let expiry_margin =
        chrono::Duration::seconds(config.reward_min_expiry_margin_secs.min(i64::MAX as u64) as i64);

    let coupons = unassigned_coupons(
        config,
        decparty,
        Some(token.to_string()),
        test_mode,
        packages,
    )
    .await?;
    let assignable = select_assignable(&coupons, Utc::now(), expiry_margin);
    if assignable.is_empty() {
        return Ok(()); // nothing assignable -> no-op
    }

    let assigned = drain_assignable(
        &assignable,
        chunk_size(config.reward_max_creates, delegation.beneficiary_count),
        MAX_SINGLE_COUPON_FAILURES,
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
    Ok(())
}

/// Assign `assignable` in successive chunks via `submit`, returning how many
/// coupons were assigned. The submit step is injected so the chunking, halving
/// and give-up rules are unit-testable without a ledger.
///
/// Chunks start at `initial_chunk` and halve on failure down to one coupon, so
/// an oversized chunk converges on a size the ledger accepts. A failure at a
/// single coupon is treated as contention rather than size: that coupon is
/// skipped, and after `max_single_failures` of them the drain stops so the
/// caller's next tick re-reads the ledger instead of grinding through a set
/// another assigner already took. A failed chunk never aborts the rest.
async fn drain_assignable<'a, F, Fut>(
    assignable: &'a [String],
    initial_chunk: usize,
    max_single_failures: usize,
    decparty: &CantonId,
    mut submit: F,
) -> usize
where
    F: FnMut(&'a str, &'a [String]) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut size = initial_chunk.max(1);
    let mut offset = 0;
    let mut assigned = 0;
    let mut single_coupon_failures = 0;
    while offset < assignable.len() {
        let end = (offset + size).min(assignable.len());
        let (primary, additional) = assignable[offset..end]
            .split_first()
            .expect("offset < len, so the chunk is non-empty");
        // Halve from what was actually submitted, not from `size`. A `size`
        // larger than the remaining set produces a smaller chunk, and halving
        // `size` would then resubmit the identical command until `size` decayed
        // below the set length — log2(size) wasted attempts per contended tick.
        let submitted = end - offset;
        match submit(primary, additional).await {
            Ok(()) => {
                assigned += submitted;
                offset = end;
            }
            Err(e) if submitted > 1 => {
                size = submitted / 2;
                tracing::warn!(%decparty, error = %e, new_chunk = size, "assign chunk failed; retrying smaller");
            }
            Err(e) => {
                single_coupon_failures += 1;
                tracing::warn!(%decparty, error = %e, coupon = %primary, "assign failed for a single coupon; skipping");
                offset += 1;
                if single_coupon_failures >= max_single_failures {
                    tracing::warn!(
                        %decparty,
                        assigned,
                        remaining = assignable.len() - offset,
                        "too many single-coupon failures; ending tick to re-read the ledger"
                    );
                    break;
                }
            }
        }
    }
    assigned
}

// ============================================================================
// Background loop + registration
// ============================================================================

/// Per-node background loop: every `reward_automation_interval_secs`, read the
/// active `CouponReassignmentDelegation` for each decparty this node holds
/// credentials for, and — if this node's member party is a listed assigner —
/// reassign its due coupons via [`run_reassign_once`]. Enablement is
/// on-ledger — a decparty with no active delegation is skipped.
pub(crate) async fn run_reward_automation_loop(data: actix_web::web::Data<AppState>) {
    // `tokio::time::interval` panics on a zero period, which would silently kill
    // this background task; clamp a misconfigured 0 to 1s and warn.
    let interval_secs = data.config.reward_automation_interval_secs;
    if interval_secs == 0 {
        tracing::warn!("reward_automation_interval_secs is 0; using 1s instead");
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
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

/// One reassign pass for a single decparty under the delegation model. No-op
/// unless the decparty has an active `CouponReassignmentDelegation` (the
/// enablement signal) naming this node's member party as an assigner.
async fn run_once_for_party(
    data: &actix_web::web::Data<AppState>,
    decparty: &CantonId,
) -> anyhow::Result<()> {
    let pkgs = packages();
    let Some((token, member)) = get_party_credentials(data, decparty).await else {
        return Ok(());
    };
    // Enablement: exactly one active delegation. None => off (no-op). >1 => Err (refuse+alert).
    let Some(delegation) =
        active_delegation(&data.config, &pkgs, data.test_mode, decparty, &token).await?
    else {
        return Ok(());
    };
    // This node must be a listed assigner, else it cannot reassign (spec §9, §11).
    if !delegation.assigners.contains(&member) {
        tracing::debug!(%decparty, %member, "node not an assigner on the delegation — skipping");
        return Ok(());
    }
    run_reassign_once(
        &data.config,
        decparty,
        &member,
        &token,
        &delegation,
        data.test_mode,
        &pkgs,
    )
    .await
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

    #[test]
    fn parse_delegation_record_reads_assigners_and_split() {
        // List values: `value::Sum::List(List { elements: vec![Value, ..] })` — same as the
        // existing parse_split_record test. `party(..)` returns value::Sum, so wrap each with
        // `value(..)`; `beneficiary_record(..)` already returns a Value. `field(label, sum)`
        // takes a value::Sum, so pass `value::Sum::List(..)` directly.
        let rec = record(vec![
            field("decparty", party(GOV)),
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
    }

    #[test]
    fn parse_delegation_record_refuses_an_empty_split() {
        // DAML rejects an empty split at create, so this is a decode problem;
        // accepting it would size chunks off a meaningless beneficiary count.
        let rec = record(vec![
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

    // ---- unassigned-coupon fail-safe filter (parse_unassigned_coupon) -------

    #[test]
    fn parse_unassigned_coupon_keeps_only_unassigned_for_decparty() {
        let alice = CantonId::parse(ALICE).unwrap();
        let bob = CantonId::parse(BOB).unwrap();

        let unassigned = record(vec![
            field("provider", party(ALICE)),
            field("beneficiary", optional_none()),
            field("amount", numeric("100.0")),
            field("expiresAt", timestamp(1_700_000_000_000_000)),
        ]);

        // provider == decparty and beneficiary is None -> kept.
        let got = parse_unassigned_coupon("00c1", &unassigned, &alice).unwrap();
        let coupon = got.expect("unassigned coupon for the decparty is kept");
        assert_eq!(coupon.cid, "00c1");
        assert_eq!(coupon.amount, "100.0".parse().unwrap());

        // provider != decparty -> skipped (fail-safe).
        assert!(
            parse_unassigned_coupon("00c1", &unassigned, &bob)
                .unwrap()
                .is_none()
        );

        // beneficiary already set -> skipped (never reassign an assigned coupon).
        let assigned = record(vec![
            field("provider", party(ALICE)),
            field("beneficiary", optional_some_party(BOB)),
            field("amount", numeric("100.0")),
            field("expiresAt", timestamp(1_700_000_000_000_000)),
        ]);
        assert!(
            parse_unassigned_coupon("00c2", &assigned, &alice)
                .unwrap()
                .is_none()
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
            provider: CantonId::parse(ALICE).expect("valid canton id"),
            amount: "1".parse().expect("valid decimal"),
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
        // A tick drains the whole set in chunks, so selection returns all of it;
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
        // transactions in ONE tick, not one transaction and a wait.
        let all = cids(120);
        let (seen, submit) = recorder(vec![]);
        let assigned = drain_assignable(&all, 50, 3, &alice(), submit).await;
        assert_eq!(assigned, 120);
        assert_eq!(*seen.borrow(), vec![50, 50, 20]);
    }

    #[tokio::test]
    async fn drain_halves_the_chunk_on_failure() {
        // An oversized chunk must converge on a size the ledger accepts rather
        // than failing identically forever. First two attempts rejected: 40 ->
        // 20 -> 10, then 10-coupon chunks succeed.
        let all = cids(40);
        let (seen, submit) = recorder(vec![false, false]);
        let assigned = drain_assignable(&all, 40, 3, &alice(), submit).await;
        assert_eq!(assigned, 40);
        assert_eq!(*seen.borrow(), vec![40, 20, 10, 10, 10, 10]);
    }

    #[tokio::test]
    async fn drain_gives_up_after_repeated_single_coupon_failures() {
        // Everything fails: halve 4 -> 2 -> 1, then three single-coupon failures
        // end the tick so the next one re-reads the ledger. Without the budget
        // this would grind through all 100 coupons one at a time.
        let all = cids(100);
        let (seen, submit) = recorder(vec![false; 100]);
        let assigned = drain_assignable(&all, 4, 3, &alice(), submit).await;
        assert_eq!(assigned, 0);
        // 4, 2, then 1 three times -> 5 attempts, then stop.
        assert_eq!(*seen.borrow(), vec![4, 2, 1, 1, 1]);
    }

    #[tokio::test]
    async fn drain_does_not_resubmit_an_identical_chunk_when_the_set_is_smaller() {
        // Regression (devnet, 2026-07-27): with one assignable coupon and a
        // chunk of 50, the submitted chunk is 1 either way, so halving `size`
        // resent the SAME command 5 times before the give-up path could fire.
        // Contention must cost one attempt, not log2(chunk).
        let all = cids(1);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 50, 3, &alice(), submit).await;
        assert_eq!(assigned, 0);
        assert_eq!(
            *seen.borrow(),
            vec![1],
            "one attempt, not a halving cascade"
        );
    }

    #[tokio::test]
    async fn drain_skips_a_poisoned_coupon_and_keeps_going() {
        // A single contended coupon must not cost the coupons behind it. Chunk
        // of 1: first fails and is skipped, the remaining two succeed.
        let all = cids(3);
        let (seen, submit) = recorder(vec![false]);
        let assigned = drain_assignable(&all, 1, 3, &alice(), submit).await;
        assert_eq!(assigned, 2);
        assert_eq!(*seen.borrow(), vec![1, 1, 1]);
    }

    #[test]
    fn chunk_size_never_stalls_a_tick() {
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
        assert_eq!(labels, ["assigner", "primaryCoupon", "additionalCoupons"]);
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
}
