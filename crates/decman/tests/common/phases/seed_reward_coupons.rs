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

/// Amulet amount for the seeded coupon; the 0.8/0.2 split yields 80.0 / 20.0.
const SEED_AMOUNT: &str = "100.0";

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

    // A freshly issued coupon at the real 36h TTL: well clear of the 2h minting
    // margin, so select_batch takes it on the next tick.
    let expires_at = (Utc::now() + chrono::Duration::hours(36))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let seed = SeedCoupon {
        dso,
        provider: decparty.clone(),
        amount: SEED_AMOUNT.to_string(),
        expires_at,
        round: 0,
    };
    let cmd = reward_coupon_create_command(&seed, &format!("seed-coupon-{}", f.run_id));
    f.submit_create(P1_JSON_API, &cmd)
        .await
        .context("create seeded RewardCouponV2")?;

    info!(
        "seed_reward_coupons: created 1 unassigned RewardCouponV2 for {decparty} \
         (beneficiary={beneficiary_party}, operator={operator_party})"
    );
    f.reward_beneficiary_party = Some(beneficiary_party);
    f.reward_operator_party = Some(operator_party);
    Ok(())
}
