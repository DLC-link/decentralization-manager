// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The CouponReassignmentDelegation read and the reward status.

use anyhow::{Context, anyhow};
use canton_proto_rs::com::daml::ledger::api::v2::Record;

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
};

use std::collections::{HashMap, HashSet};

use crate::server::record::{
    extract_list, extract_record, field_decimal, field_list_len, field_party_id, field_party_list,
    get_field,
};
use crate::server::{
    package_inventory::fetch_package_id_to_name,
    types::{CouponReassignmentDelegationSummary, RewardBeneficiary, RewardStatusResponse},
};

use super::{
    assign::package_absent,
    ledger::{ContractFilter, Entity, Module, bounded_created_records, for_each_active_created},
    scheduler::{Role, role_for},
};

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

impl From<ActiveDelegation> for CouponReassignmentDelegationSummary {
    fn from(d: ActiveDelegation) -> Self {
        Self {
            cid: d.cid,
            dso: d.dso,
            assigners: d.assigners,
            beneficiary_count: d.beneficiary_count,
        }
    }
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

    let records = bounded_created_records(
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

    let records = bounded_created_records(
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

/// Every decparty whose active delegation names `member` as an assigner.
///
/// Read as `member`, not as the decparty. `CouponReassignmentDelegation` makes
/// its `assigners` observers, so this node sees exactly the delegations it is
/// named on, under a token it already holds for some other decparty. That is
/// what lets the loop tell a decparty whose credentials matter from one that was
/// never enabled: the enablement signal lives behind the decparty's own
/// credentials, and those are what failed.
pub(super) async fn delegated_decparties(
    config: &NodeConfig,
    packages: &PackageConfig,
    test_mode: bool,
    member: &CantonId,
    token: &str,
) -> anyhow::Result<HashSet<CantonId>> {
    let Some(package_id) = packages.governance_rewards.as_deref() else {
        return Ok(HashSet::new());
    };
    let mut decparties = HashSet::new();
    for_each_active_created(
        config,
        member,
        Some(token.to_string()),
        test_mode,
        ContractFilter::template(
            package_id,
            Module("Governance.Rewards.CouponReassignmentDelegation"),
            Entity("CouponReassignmentDelegation"),
        ),
        |_, _, rec| {
            // A delegation that names no decparty we can decode is not
            // enablement for anyone, so it is skipped rather than failing the
            // whole read: one bad contract must not silence every good one.
            if let Ok(decparty) = field_party_id(rec, "decparty") {
                decparties.insert(decparty);
            }
            Ok(())
        },
    )
    .await?;
    Ok(decparties)
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
// Reward status (the dec party page)
// ============================================================================

/// The decparty's reward status, as the dec party page shows it (#356).
///
/// It reads the delegation the sweep acts on. On a participant without the
/// rewards DAR that read fails with [`package_absent`]. The loop goes quiet on
/// that error, so this page is where an operator learns of it.
pub(crate) async fn reward_status(
    config: &NodeConfig,
    packages: &PackageConfig,
    test_mode: bool,
    decparty: &CantonId,
    member: &CantonId,
    token: &str,
) -> anyhow::Result<RewardStatusResponse> {
    let package_id = packages
        .governance_rewards
        .as_deref()
        .context("governance_rewards package not configured")?;
    // Under mock auth the read below is a wildcard, so Canton never resolves
    // the rewards package, and a missing DAR would read as no delegation.
    if test_mode && !package_listed(&fetch_package_id_to_name(config).await?, package_id) {
        return Ok(missing_dar_status(member));
    }
    let read = bounded_created_records(
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
    .await;
    reward_status_from(read, decparty, member)
}

/// Whether `package_ref` is among `packages` (id to name). A `#name`
/// reference matches by name, anything else by package id.
fn package_listed(packages: &HashMap<String, String>, package_ref: &str) -> bool {
    match package_ref.strip_prefix('#') {
        Some(name) => packages.values().any(|listed| listed == name),
        None => packages.contains_key(package_ref),
    }
}

/// The status of a participant without the rewards DAR.
fn missing_dar_status(member: &CantonId) -> RewardStatusResponse {
    RewardStatusResponse {
        rewards_dar_resolvable: false,
        delegation: None,
        member_party_id: member.clone(),
        member_is_assigner: false,
        beneficiaries: Vec::new(),
    }
}

/// Turn a delegation read into the reward status (pure).
///
/// Only an absent package reads as a missing DAR. Every other failure stays an
/// error, so the page reports a failed read instead of a state it never saw.
fn reward_status_from(
    read: anyhow::Result<Vec<(String, i64, Record)>>,
    decparty: &CantonId,
    member: &CantonId,
) -> anyhow::Result<RewardStatusResponse> {
    let records = match read {
        Ok(records) => records,
        Err(e) if package_absent(&e) => return Ok(missing_dar_status(member)),
        Err(e) => return Err(e),
    };
    let mut status = RewardStatusResponse {
        rewards_dar_resolvable: true,
        ..missing_dar_status(member)
    };
    let Some((cid, rec)) = newest_delegation_for(records, decparty) else {
        return Ok(status);
    };
    let delegation = parse_delegation_record(&cid, &rec)?;
    status.beneficiaries = parse_split(&rec)?;
    status.member_is_assigner = role_for(&delegation, member) == Role::Assign;
    status.delegation = Some(delegation.into());
    Ok(status)
}

/// Decode a delegation's `split` for display. Nothing builds a command from
/// it: `Delegation_Assign` always uses the split stored on the contract.
fn parse_split(rec: &Record) -> anyhow::Result<Vec<RewardBeneficiary>> {
    extract_list(get_field(rec, "split")?)?
        .elements
        .iter()
        .map(|entry| {
            let entry = extract_record(entry)?;
            Ok(RewardBeneficiary {
                beneficiary: field_party_id(entry, "beneficiary")?,
                percentage: field_decimal(entry, "percentage")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        ALICE, BOB, CAROL, GOV, canton_err, field, numeric, party, record, value,
    };
    use super::*;
    use anyhow::anyhow;
    use canton_proto_rs::com::daml::ledger::api::v2::{List, Record, Value, value};

    fn beneficiary_record(p: &str, pct: &str) -> Value {
        value(value::Sum::Record(record(vec![
            field("beneficiary", party(p)),
            field("percentage", numeric(pct)),
        ])))
    }

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
    fn newest_delegation_wins_regardless_of_read_order() -> anyhow::Result<()> {
        // The ACS read order is not creation order. Acting on an older
        // delegation pays rewards to a split a later vote replaced, so every
        // read order must pick the newest. The six orders of three records are
        // the three rotations of the list and of its reverse.
        let gov = CantonId::parse(GOV)?;
        let delegations = [("00old", 100), ("00mid", 500), ("00new", 900)];
        for reversed in [false, true] {
            for rotation in 0..delegations.len() {
                let mut order = delegations.to_vec();
                if reversed {
                    order.reverse();
                }
                order.rotate_left(rotation);
                let records = order
                    .iter()
                    .map(|(cid, offset)| (cid.to_string(), *offset, delegation_of(GOV)))
                    .collect();
                let (cid, _) = newest_delegation_for(records, &gov)
                    .ok_or_else(|| anyhow!("no delegation selected from {order:?}"))?;
                assert_eq!(
                    cid, "00new",
                    "read order {order:?} acted on a superseded split"
                );
            }
        }
        Ok(())
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

    /// A delegation for GOV that lists `assigners` and holds `split`.
    fn delegation_listing(assigners: &[&str], split: Vec<Value>) -> Record {
        let assigners = assigners.iter().map(|a| value(party(a))).collect();
        record(vec![
            field("decparty", party(GOV)),
            field("dso", party(GOV)),
            field(
                "assigners",
                value::Sum::List(List {
                    elements: assigners,
                }),
            ),
            field("split", value::Sum::List(List { elements: split })),
        ])
    }

    fn status_for(
        read: anyhow::Result<Vec<(String, i64, Record)>>,
        member: &str,
    ) -> anyhow::Result<RewardStatusResponse> {
        reward_status_from(read, &CantonId::parse(GOV)?, &CantonId::parse(member)?)
    }

    #[test]
    fn only_an_absent_package_reads_as_a_missing_dar() -> anyhow::Result<()> {
        let status = status_for(Err(canton_err("PACKAGE_NAMES_NOT_FOUND")), ALICE)?;
        assert!(!status.rewards_dar_resolvable);
        assert!(status.delegation.is_none() && status.beneficiaries.is_empty());
        // Any other failure shows neither a missing DAR nor a missing delegation.
        assert!(status_for(Err(canton_err("PERMISSION_DENIED")), ALICE).is_err());
        assert!(status_for(Err(anyhow!("connection refused")), ALICE).is_err());
        Ok(())
    }

    /// Mock auth reads the delegation with a wildcard, so the DAR check
    /// looks the package up on the participant instead.
    #[test]
    fn a_package_reference_matches_by_name_or_by_id() {
        let packages: HashMap<String, String> = [
            (
                "pkg-rewards".to_string(),
                "governance-rewards-automation-v1".to_string(),
            ),
            ("pkg-core".to_string(), "governance-core-v1".to_string()),
        ]
        .into_iter()
        .collect();

        assert!(package_listed(
            &packages,
            "#governance-rewards-automation-v1"
        ));
        assert!(package_listed(&packages, "pkg-rewards"));
        assert!(!package_listed(&packages, "#governance-rewards-v1"));
        assert!(!package_listed(&packages, "pkg-missing"));
        // A name is never matched as an id, nor an id as a name.
        assert!(!package_listed(
            &packages,
            "governance-rewards-automation-v1"
        ));
        assert!(!package_listed(&packages, "#pkg-rewards"));
    }

    #[test]
    fn a_decparty_without_a_delegation_has_the_dar_and_no_delegation() -> anyhow::Result<()> {
        let theirs = vec![("00theirs".to_string(), 999, delegation_of(BOB))];
        for read in [vec![], theirs] {
            let status = status_for(Ok(read), ALICE)?;
            assert!(status.rewards_dar_resolvable);
            assert!(status.delegation.is_none() && !status.member_is_assigner);
        }
        Ok(())
    }

    #[test]
    fn the_newest_delegation_says_whether_this_member_assigns() -> anyhow::Result<()> {
        // The older delegation lists ALICE only, so picking it would deny BOB.
        let split = vec![
            beneficiary_record(ALICE, "0.8"),
            beneficiary_record(BOB, "0.2"),
        ];
        let read = || {
            Ok(vec![
                ("00old".to_string(), 100, delegation_of(GOV)),
                (
                    "00new".to_string(),
                    900,
                    delegation_listing(&[ALICE, BOB], split.clone()),
                ),
            ])
        };
        let bob = status_for(read(), BOB)?;
        assert!(bob.member_is_assigner);
        assert_eq!(bob.delegation.context("a delegation")?.cid, "00new");
        let shares: Vec<(String, String)> = bob
            .beneficiaries
            .iter()
            .map(|b| (b.beneficiary.to_string(), b.percentage.to_string()))
            .collect();
        assert_eq!(
            shares,
            [(ALICE, "0.8"), (BOB, "0.2")].map(|(p, s)| (p.to_string(), s.to_string()))
        );
        // CAROL is not listed. She still sees the delegation and its split.
        let carol = status_for(read(), CAROL)?;
        assert!(!carol.member_is_assigner);
        assert!(carol.delegation.is_some());
        assert_eq!(carol.beneficiaries.len(), 2);
        Ok(())
    }

    #[test]
    fn a_split_that_does_not_decode_is_an_error_not_an_empty_split() {
        let no_share = value(value::Sum::Record(record(vec![field(
            "beneficiary",
            party(ALICE),
        )])));
        let read = vec![(
            "00del".to_string(),
            1,
            delegation_listing(&[ALICE], vec![no_share]),
        )];
        assert!(status_for(Ok(read), ALICE).is_err());
    }
}
