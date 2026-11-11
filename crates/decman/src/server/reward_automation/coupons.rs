// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Unassigned reward coupons and coupon batch selection.

use canton_proto_rs::com::daml::ledger::api::v2::Record;
use chrono::{DateTime, Utc};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
};

use crate::server::record::{field_decimal, field_optional_is_none, field_party_id, field_time};

use super::ledger::{ContractFilter, Entity, Module, for_each_active_created};

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
    let mut coupons = Vec::new();
    for_each_unassigned_coupon(
        config,
        decparty,
        dso,
        token,
        test_mode,
        packages,
        |coupon| {
            coupons.push(coupon);
            Ok(())
        },
    )
    .await?;
    Ok(coupons)
}

/// The streaming half of [`unassigned_coupons`]: decode each `RewardCoupon`
/// interface view as it arrives and hand the unassigned ones to `visit`.
///
/// The decode and the unassigned/dso/provider filter happen per record, so the
/// verbose `Record` is dropped before the next one is read and only what
/// `visit` keeps survives. A caller that needs no list at all — see
/// [`read_oldest_expiry`], which wants one `min` — then allocates nothing per
/// coupon. Collecting the records first is what OOMKilled a node (#413).
pub(super) async fn for_each_unassigned_coupon<F>(
    config: &NodeConfig,
    decparty: &CantonId,
    dso: &CantonId,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
    mut visit: F,
) -> anyhow::Result<()>
where
    F: FnMut(CouponInfo) -> anyhow::Result<()>,
{
    let _ = packages; // the interface is resolved by name-alias, not PackageConfig.
    for_each_active_created(
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
        |cid, _, rec| {
            if let Some(coupon) = parse_unassigned_coupon(cid, rec, decparty, dso)? {
                visit(coupon)?;
            }
            Ok(())
        },
    )
    .await
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
///
/// Takes the coupons by value and moves each cid out rather than cloning it: on
/// a decparty with a large backlog the caller has no use for the list
/// afterwards, and holding a second copy of every cid was part of what put the
/// sweep's peak over a node's memory limit (#413).
pub(crate) fn select_assignable(
    mut coupons: Vec<CouponInfo>,
    now: DateTime<Utc>,
    expiry_margin: chrono::Duration,
) -> Vec<String> {
    coupons.retain(|c| is_assignable(c, now, expiry_margin));
    coupons.sort_by_key(|c| c.expires_at);
    coupons.into_iter().map(|c| c.cid).collect()
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        ALICE, BOB, GOV, coupon, dt, field, numeric, party, record, timestamp, value,
    };
    use super::*;
    use canton_proto_rs::com::daml::ledger::api::v2::{Optional, value};

    fn optional_none() -> value::Sum {
        value::Sum::Optional(Box::new(Optional { value: None }))
    }

    fn optional_some_party(p: &str) -> value::Sum {
        value::Sum::Optional(Box::new(Optional {
            value: Some(Box::new(value(party(p)))),
        }))
    }

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
        let got = select_assignable(coupons, now, chrono::Duration::minutes(2));
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
        let got = select_assignable(coupons, now, chrono::Duration::minutes(2));
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
            select_assignable(coupons, now, chrono::Duration::hours(2)).len(),
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

    #[test]
    fn chunk_size_never_stalls_a_sweep() {
        // A split wider than the create budget still yields a 1-coupon chunk
        // rather than 0, which would loop forever making no progress.
        assert_eq!(chunk_size(100, 500), 1);
        assert_eq!(chunk_size(0, 2), 1);
        // A malformed 0 count must not divide by zero.
        assert_eq!(chunk_size(100, 0), 100);
    }
}
