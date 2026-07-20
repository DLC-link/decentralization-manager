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

// The reader side of the automation lands ahead of the proposer/confirmer
// roles and the background loop (M3+M4 Tasks 5–9) that consume it, so several
// `pub(crate)` items are not yet wired into a call path. Mirrors the same
// `#[allow(dead_code)]` on `resolve_active_governance_rules` in governance.rs.
#![allow(dead_code)]

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

use super::types::RewardBeneficiary;

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
// RewardSplitConfig (the on-ledger split)
// ============================================================================

/// Decode a `RewardSplitConfig` create-arguments `Record` into the configured
/// split. Reads the `beneficiaries` field — a list of
/// `{ beneficiary : Party, percentage : Numeric }` records.
pub(crate) fn parse_split_record(rec: &Record) -> anyhow::Result<Vec<RewardBeneficiary>> {
    let list = match record_field(rec, "beneficiaries") {
        Some(value::Sum::List(l)) => l,
        _ => {
            return Err(anyhow!(
                "RewardSplitConfig record missing `beneficiaries` list"
            ));
        }
    };

    let mut out = Vec::with_capacity(list.elements.len());
    for elem in &list.elements {
        let inner = match elem.sum.as_ref() {
            Some(value::Sum::Record(r)) => r,
            _ => return Err(anyhow!("`beneficiaries` element is not a record")),
        };
        out.push(RewardBeneficiary {
            beneficiary: field_party_id(inner, "beneficiary")?,
            percentage: field_decimal(inner, "percentage")?,
        });
    }
    Ok(out)
}

/// Reads the effective reward split for a decparty.
#[async_trait::async_trait]
pub(crate) trait SplitSource {
    /// The configured split, or `None` when the decparty has no
    /// `RewardSplitConfig` (i.e. the automation is not enabled for it).
    async fn effective_split(
        &self,
        decparty: &CantonId,
        token: &str,
    ) -> anyhow::Result<Option<Vec<RewardBeneficiary>>>;
}

/// A [`SplitSource`] backed by the on-ledger `RewardSplitConfig` contract.
pub(crate) struct OnLedgerSplitSource<'a> {
    config: &'a NodeConfig,
    packages: &'a PackageConfig,
    test_mode: bool,
}

impl<'a> OnLedgerSplitSource<'a> {
    pub(crate) fn new(
        config: &'a NodeConfig,
        packages: &'a PackageConfig,
        test_mode: bool,
    ) -> Self {
        Self {
            config,
            packages,
            test_mode,
        }
    }
}

#[async_trait::async_trait]
impl SplitSource for OnLedgerSplitSource<'_> {
    async fn effective_split(
        &self,
        decparty: &CantonId,
        token: &str,
    ) -> anyhow::Result<Option<Vec<RewardBeneficiary>>> {
        let Some(package_id) = self.packages.governance_rewards.as_deref() else {
            return Ok(None);
        };

        let records = active_created_records(
            self.config,
            decparty,
            Some(token.to_string()),
            self.test_mode,
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
            .filter(|(_, rec)| {
                field_party_id(rec, "governanceParty").ok().as_ref() == Some(decparty)
            })
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
}

// ============================================================================
// Unassigned reward coupons
// ============================================================================

/// A decparty's unassigned reward coupon (`RewardCoupon` interface view).
pub(crate) struct CouponInfo {
    pub cid: String,
    pub provider: CantonId,
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

#[cfg(test)]
mod tests {
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
}
