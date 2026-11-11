// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Reward automation metrics.

use prometheus::{GaugeVec, IntCounter, IntCounterVec};

use std::sync::LazyLock;

// ============================================================================
// Metrics
// ============================================================================

// The reward automation's health signal.

pub(super) static HEARTBEAT: LazyLock<IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(
        "decman_reward_heartbeat_total",
        "Heartbeats of the reward automation loop, one per wake while it lives — every 60s, or every sweep interval when that is shorter."
    )
    .expect("metric name is a unique literal")
});

pub(super) static SWEEPS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_sweep_total",
        "Reward sweeps started for a decparty.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static ASSIGNED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_coupons_assigned_total",
        "Coupons successfully reassigned by this node.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static SKIPPED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_coupons_skipped_total",
        "Coupons the ledger rejected individually, to be retried next sweep.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static ZERO_ASSIGNED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_sweep_zero_assigned_total",
        "Reward sweeps that found unassigned coupons and assigned none of them.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static SWEEP_FAILED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_sweep_failed_total",
        "Reward sweeps that ended in an error.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static EXPIRY_READS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_expiry_read_total",
        "Backlog reads that refreshed the expiry gauge without assigning anything.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static CREDENTIALS_UNAVAILABLE: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_credentials_unavailable_total",
        "Passes whose token fetch failed for a decparty whose delegation names this node as an assigner.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static AUTH_UNREGISTERED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_auth_unregistered_total",
        "Passes that found no auth registry entry for a decparty whose delegation names this node as an assigner.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

pub(super) static ASSIGNER_NOT_MEMBER: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_reward_assigner_not_member_total",
        "Sweeps that found an assigner on the delegation who is no longer a governance member of the decparty.",
        &["decparty"]
    )
    .expect("metric name is a unique literal")
});

// Carries no decparty label: one enablement read serves every decparty this
// node holds credentials for, and a pass that fails reads none of them.
pub(super) static ENABLEMENT_UNREADABLE: LazyLock<IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(
        "decman_reward_enablement_unreadable_total",
        "Passes that could not read which decparties delegate to this node, so no auth fault was attributable to any of them."
    )
    .expect("metric name is a unique literal")
});

pub(super) static OLDEST_EXPIRES_IN: LazyLock<GaugeVec> = LazyLock::new(|| {
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
    LazyLock::force(&CREDENTIALS_UNAVAILABLE);
    LazyLock::force(&AUTH_UNREGISTERED);
    LazyLock::force(&ASSIGNER_NOT_MEMBER);
    LazyLock::force(&ENABLEMENT_UNREADABLE);
    LazyLock::force(&OLDEST_EXPIRES_IN);
}

/// Exports the per-decparty counters at zero for a decparty this node sweeps. A
/// counter that only appears on its first event leaves a dashboard unable to tell
/// a node that never failed from an instrument that is missing.
pub(super) fn ensure_outcome_series(label: &str) {
    for counter in [
        &*ASSIGNED,
        &*SKIPPED,
        &*ZERO_ASSIGNED,
        &*SWEEP_FAILED,
        &*EXPIRY_READS,
        &*CREDENTIALS_UNAVAILABLE,
        &*AUTH_UNREGISTERED,
        &*ASSIGNER_NOT_MEMBER,
    ] {
        counter.with_label_values(&[label]).inc_by(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gathered_family_names() -> Vec<String> {
        prometheus::gather()
            .into_iter()
            .map(|f| f.name().to_string())
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
    fn the_unreadable_enablement_counter_is_exposed_before_any_pass_runs() {
        // Unlabelled, so `gather()` exports it from boot. An alert on its
        // increase needs a zero to increase from.
        register_metrics();
        assert!(
            gathered_family_names()
                .iter()
                .any(|n| n == "decman_reward_enablement_unreadable_total")
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
        // so a served decparty exports all eight at zero from its first sweep.
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
            "decman_reward_credentials_unavailable_total",
            "decman_reward_auth_unregistered_total",
            "decman_reward_assigner_not_member_total",
        ] {
            let family = families
                .iter()
                .find(|f| f.name() == name)
                .unwrap_or_else(|| panic!("{name} is missing from the registry"));
            let metric = family
                .get_metric()
                .iter()
                .find(|m| m.get_label().iter().any(|l| l.value() == label))
                .unwrap_or_else(|| panic!("{name} has no series for the decparty"));
            assert_eq!(
                metric.get_counter().get_value(),
                0.0,
                "{name} should read zero"
            );
        }
    }
}
