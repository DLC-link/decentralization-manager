//! Localnet-only: seed unassigned `RewardCouponV2` coupons so the CIP-104 Mode A
//! coupon-reassignment phase can exercise the assign + 0.8/0.2 split
//! deterministically in CI — no devnet, no reward-round issuance.
//!
//! On localnet the harness controls the DSO. `RewardCouponV2`'s sole signatory
//! is `dso`, so we allocate a dedicated DSO party and create the coupons
//! directly via the participant's JSON Ledger API as `ledger-api-user` acting
//! as it. The delegation then names that party as its `dso`. Keeping it
//! distinct from every member party is what lets one more coupon, minted by a
//! member instead, stand for a coupon from a foreign DSO (see
//! [`FOREIGN_TTL_HOURS`]).
//! `provider = decparty` + `providerIsObserver = true` makes each coupon
//! visible to the decparty (the reassignment authority), and
//! `beneficiary = null` marks it unassigned (what the automation looks for).
//! `expiresAt = now + 36h` mirrors a freshly issued coupon at the real TTL, so
//! the phase exercises the automation on the same shape production sees.
//! Beneficiaries are two fresh non-assigner parties, matching the real
//! cbtc-network topology.

use anyhow::Context;
use chrono::Utc;
use tracing::info;

use crate::common::{
    Fixture, TestTarget,
    ledger_api::{P1_JSON_API, SeedCoupon, reward_coupon_create_command},
    phases::deploy_gov_core::{allocate_party, grant_rights},
};

/// Amulet amount per seeded coupon; the 0.8/0.2 split yields 80.0 / 20.0 each.
pub const SEED_AMOUNT: f64 = 100.0;

/// How many unassigned coupons to seed.
///
/// Deliberately **more than one chunk**: `run_reassign_once` sizes a chunk in
/// output creates (`MAX_CREATES / beneficiary_count` = 100/2 = 50 coupons here),
/// so 60 forces the drain loop to submit a second `Delegation_Assign` — the
/// multi-chunk path a single-coupon seed never reaches. (That the whole set
/// drains within *one* sweep is pinned down by the `drain_assignable` unit tests;
/// an e2e poll cannot distinguish one draining sweep from several chunking ones.)
pub const SEED_COUPON_COUNT: usize = 60;

/// Seeded coupons on top of [`SEED_COUPON_COUNT`] that every sweep leaves
/// unassigned: the unassignable one and the foreign-DSO one.
pub const SEEDED_SURVIVORS: usize = 2;

/// Hours until every coupon the DSO mints expires: a fresh coupon's real TTL.
pub const SEED_TTL_HOURS: i64 = 36;

/// Hours until the foreign-DSO coupon expires.
///
/// That coupon names `provider = decparty` like the rest, but a member party
/// minted it as `dso`. `RewardCouponV2` lets anyone do that, so the reward
/// automation must leave it alone. Two layers enforce it: the coupon reader
/// drops it, and `Delegation_Assign` asserts on it.
///
/// It expires before every other seeded coupon, so it is the most urgent coupon
/// a reader could admit. A reader that admitted it would report its expiry on
/// the oldest-unassigned gauge, and would put it first in the first chunk. The
/// coupon-reassignment phase relies on both.
pub const FOREIGN_TTL_HOURS: i64 = 12;

/// Coupons per seed transaction. Keeps each seeding submission near the size
/// devnet has proven (72 creates) so the seed itself is never the thing that
/// fails when the point of the test is what the automation does afterwards.
const SEED_TX_SIZE: usize = 20;

/// Amount for one coupon the ledger admits but refuses to assign, seeded
/// alongside the healthy set so the drain's fan-out runs against a real ledger.
///
/// `Delegation_Assign` creates one coupon per beneficiary at
/// `amount * percentage`, and `RewardCouponV2` carries `ensure amount > 0.0`.
/// Daml `Decimal` holds 10 decimal places, so `1e-10 * 0.2` rounds to `0.0` and
/// that create fails, taking its whole chunk with it. The reader still admits
/// the coupon — right `provider`, right `dso`, `beneficiary` null, `amount`
/// decodes — so it reaches a batch and the fan-out has to isolate it.
///
/// **This amount cannot occur in production.** Devnet's smallest real coupon is
/// 4.31 CC against a threshold near 2.5e-10. The point is to exercise the
/// mechanism, which unit tests alone cover, not to model a live risk.
pub const UNASSIGNABLE_AMOUNT: &str = "0.0000000001";

fn expires_in(hours: i64) -> String {
    (Utc::now() + chrono::Duration::hours(hours))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    if f.target != TestTarget::Localnet {
        info!("seed_reward_coupons: skipped (localnet-only)");
        return Ok(());
    }
    info!("Phase: seed_reward_coupons (localnet)");

    let decparty = f.party_id()?.to_string();
    let dso = allocate_party(&*f, P1_JSON_API, "reward-dso", "participant-1").await?;
    grant_rights(&*f, P1_JSON_API, &dso, "participant-1").await?;
    // The member that mints the foreign-DSO coupon. ledger-api-user can already
    // act as p1_member on participant-1, so this needs no extra party.
    let foreign_dso = f.p1_member_party()?.to_string();

    // Two fresh non-assigner beneficiary parties on participant-1, with
    // ledger-api-user granted read so the split assertion can read their coupons.
    let beneficiary_party =
        allocate_party(&*f, P1_JSON_API, "cbtc-beneficiary", "participant-1").await?;
    let operator_party =
        allocate_party(&*f, P1_JSON_API, "reward-operator", "participant-1").await?;
    grant_rights(&*f, P1_JSON_API, &beneficiary_party, "participant-1").await?;
    grant_rights(&*f, P1_JSON_API, &operator_party, "participant-1").await?;

    // Freshly issued coupons at the real 36h TTL: well clear of the expiry
    // margin, so select_assignable takes them on the next sweep. Distinct rounds
    // keep the seeded contracts distinguishable in a failure dump.
    let expires_at = expires_in(SEED_TTL_HOURS);
    let mut seeds: Vec<SeedCoupon> = (0..SEED_COUPON_COUNT)
        .map(|i| SeedCoupon {
            dso: dso.clone(),
            provider: decparty.clone(),
            amount: format!("{SEED_AMOUNT:.1}"),
            expires_at: expires_at.clone(),
            round: i as i64,
        })
        .collect();
    // One coupon the ledger will admit and then refuse to assign, so the drain's
    // fan-out runs for real. See UNASSIGNABLE_AMOUNT.
    seeds.push(SeedCoupon {
        dso: dso.clone(),
        provider: decparty.clone(),
        amount: UNASSIGNABLE_AMOUNT.to_string(),
        expires_at: expires_at.clone(),
        round: SEED_COUPON_COUNT as i64,
    });
    for (batch, chunk) in seeds.chunks(SEED_TX_SIZE).enumerate() {
        let cmd =
            reward_coupon_create_command(chunk, &format!("seed-coupons-{}-{batch}", f.run_id));
        f.submit_create(P1_JSON_API, &cmd)
            .await
            .with_context(|| format!("create seeded RewardCouponV2 batch {batch}"))?;
    }

    // The foreign-DSO coupon gets its own transaction, because one transaction
    // acts as one dso. See FOREIGN_TTL_HOURS.
    let foreign = SeedCoupon {
        dso: foreign_dso.clone(),
        provider: decparty.clone(),
        amount: format!("{SEED_AMOUNT:.1}"),
        expires_at: expires_in(FOREIGN_TTL_HOURS),
        round: (SEED_COUPON_COUNT + 1) as i64,
    };
    let cmd = reward_coupon_create_command(
        std::slice::from_ref(&foreign),
        &format!("seed-foreign-coupon-{}", f.run_id),
    );
    f.submit_create(P1_JSON_API, &cmd)
        .await
        .context("create the foreign-DSO RewardCouponV2")?;
    let foreign_cids: Vec<String> = f
        .active_reward_coupons(P1_JSON_API, &decparty)
        .await?
        .into_iter()
        .filter(|c| c.dso == foreign_dso)
        .map(|c| c.contract_id)
        .collect();
    let [foreign_cid] = foreign_cids.as_slice() else {
        anyhow::bail!(
            "expected one coupon minted by {foreign_dso} for {decparty}, found {}",
            foreign_cids.len()
        );
    };

    info!(
        "seed_reward_coupons: created {SEED_COUPON_COUNT} unassigned RewardCouponV2 plus one \
         unassignable at {UNASSIGNABLE_AMOUNT} for {decparty} as DSO {dso}, and foreign-DSO \
         coupon {foreign_cid} as {foreign_dso} \
         (beneficiary={beneficiary_party}, operator={operator_party})"
    );
    f.reward_foreign_coupon_cid = Some(foreign_cid.clone());
    f.reward_dso_party = Some(dso);
    f.reward_beneficiary_party = Some(beneficiary_party);
    f.reward_operator_party = Some(operator_party);
    Ok(())
}
