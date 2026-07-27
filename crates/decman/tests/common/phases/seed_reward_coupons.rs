//! Localnet-only: seed unassigned `RewardCouponV2` coupons so the CIP-104 Mode A
//! coupon-reassignment phase can exercise the assign + 0.8/0.2 split
//! deterministically in CI — no devnet, no reward-round issuance.
//!
//! On localnet the harness controls the DSO (it substitutes `p1_member`; see
//! `token_custody`). `RewardCouponV2`'s sole signatory is `dso`, so we create
//! the coupon directly via the participant's JSON Ledger API as `ledger-api-user`
//! acting as `p1_member`. `provider = decparty` + `providerIsObserver = true`
//! makes it visible to the decparty (the reassignment authority), and
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
/// drains within *one* tick is pinned down by the `drain_assignable` unit tests;
/// an e2e poll cannot distinguish one draining tick from several chunking ones.)
pub const SEED_COUPON_COUNT: usize = 60;

/// Coupons per seed transaction. Keeps each seeding submission near the size
/// devnet has proven (72 creates) so the seed itself is never the thing that
/// fails when the point of the test is what the automation does afterwards.
const SEED_TX_SIZE: usize = 20;

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    if f.target != TestTarget::Localnet {
        info!("seed_reward_coupons: skipped (localnet-only)");
        return Ok(());
    }
    info!("Phase: seed_reward_coupons (localnet)");

    let decparty = f.party_id()?.to_string();
    let dso = f.p1_member_party()?.to_string(); // localnet DSO stand-in

    // Two fresh non-assigner beneficiary parties on participant-1, with
    // ledger-api-user granted read so the split assertion can read their coupons.
    let beneficiary_party =
        allocate_party(&*f, P1_JSON_API, "cbtc-beneficiary", "participant-1").await?;
    let operator_party =
        allocate_party(&*f, P1_JSON_API, "reward-operator", "participant-1").await?;
    grant_rights(&*f, P1_JSON_API, &beneficiary_party, "participant-1").await?;
    grant_rights(&*f, P1_JSON_API, &operator_party, "participant-1").await?;

    // Freshly issued coupons at the real 36h TTL: well clear of the 2h minting
    // margin, so select_assignable takes them on the next tick. Distinct rounds
    // keep the seeded contracts distinguishable in a failure dump.
    let expires_at = (Utc::now() + chrono::Duration::hours(36))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let seeds: Vec<SeedCoupon> = (0..SEED_COUPON_COUNT)
        .map(|i| SeedCoupon {
            dso: dso.clone(),
            provider: decparty.clone(),
            amount: format!("{SEED_AMOUNT:.1}"),
            expires_at: expires_at.clone(),
            round: i as i64,
        })
        .collect();
    for (batch, chunk) in seeds.chunks(SEED_TX_SIZE).enumerate() {
        let cmd =
            reward_coupon_create_command(chunk, &format!("seed-coupons-{}-{batch}", f.run_id));
        f.submit_create(P1_JSON_API, &cmd)
            .await
            .with_context(|| format!("create seeded RewardCouponV2 batch {batch}"))?;
    }

    info!(
        "seed_reward_coupons: created {SEED_COUPON_COUNT} unassigned RewardCouponV2 for \
         {decparty} (beneficiary={beneficiary_party}, operator={operator_party})"
    );
    f.reward_beneficiary_party = Some(beneficiary_party);
    f.reward_operator_party = Some(operator_party);
    Ok(())
}
