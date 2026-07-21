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
//!   `CouponReassignmentDelegation` (its cid and authorized `assigners`); the
//!   split itself is not read here — `Delegation_Assign` enforces it in DAML.
//! * [`unassigned_coupons`] — reads the decparty's unassigned `RewardCoupon`
//!   interface views.
//! * [`run_reassign_once`] — one reassign tick: selects a ripe batch of
//!   unassigned coupons and exercises `Delegation_Assign` for it.
//!
//! The pure record/batch decoders ([`select_batch`]) are unit-tested here; the
//! gRPC reads and command submission are exercised by the devnet integration
//! test.

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
// CouponReassignmentDelegation (the delegation-model enablement signal)
// ============================================================================

/// The active `CouponReassignmentDelegation` for a decparty: its cid and the
/// set of members authorized to execute `Delegation_Assign`. The split is
/// **not** carried here — it lives in the on-ledger contract and
/// `Delegation_Assign` enforces it by construction (spec §12), so the Rust
/// side never needs to read it.
pub(crate) struct ActiveDelegation {
    pub cid: String,
    pub assigners: Vec<CantonId>,
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
    Ok(ActiveDelegation {
        cid: cid.to_string(),
        assigners: field_party_list(rec, "assigners")?,
    })
}

/// The active `CouponReassignmentDelegation` for a decparty, read from the
/// ledger, or `None` when there is none (automation not enabled for that
/// decparty). Defends the keyless-singleton invariant, mirroring
/// `effective_split`. Replaces `effective_split` as the Task 6 loop's
/// enablement + assigners source (`effective_split` is deleted in Task 7).
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
    let template_id = Identifier {
        package_id,
        module_name: "Governance.Rewards".to_string(),
        entity_name: "CouponReassignmentDelegation".to_string(),
    };
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

/// One reassign tick for a decparty under the delegation model: read
/// unassigned coupons, select a ripe batch, and — if non-empty — exercise
/// `Delegation_Assign` for it. An empty batch (nothing ripe) is a no-op.
/// Batch policy constants mirror `run_proposer_once` (spec §9/§11).
pub(crate) async fn run_reassign_once(
    config: &NodeConfig,
    decparty: &CantonId,
    assigner: &CantonId,
    token: &str,
    delegation: &ActiveDelegation,
    test_mode: bool,
    packages: &PackageConfig,
) -> anyhow::Result<()> {
    const WATERMARK: chrono::Duration = chrono::Duration::hours(6);
    const MINTING_MARGIN: chrono::Duration = chrono::Duration::hours(2);
    const MAX_BATCH: usize = 50;
    let coupons = unassigned_coupons(
        config,
        decparty,
        Some(token.to_string()),
        test_mode,
        packages,
    )
    .await?;
    let batch = select_batch(&coupons, Utc::now(), WATERMARK, MINTING_MARGIN, MAX_BATCH);
    let Some((primary, additional)) = batch.split_first() else {
        return Ok(()); // nothing ripe -> no-op
    };
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
    .await?;
    tracing::info!(%decparty, %assigner, count = batch.len(), "reassigned coupon batch");
    Ok(())
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
        // split is not parsed (DAML-enforced) — the record's `split` field is ignored.
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

    #[test]
    fn build_delegation_assign_arg_shape() {
        // rb(..).beneficiary yields a CantonId (this module has no canton_id helper);
        // rb takes a bare prefix and appends a fixed namespace itself.
        let rec =
            build_delegation_assign_arg(&rb("m1", "1.0").beneficiary, "00c1", &["00c2".into()]);
        let labels: Vec<&str> = rec.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["assigner", "primaryCoupon", "additionalCoupons"]);
    }
}
