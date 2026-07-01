mod common;

use tracing::field::{Field, Visit};
use tracing_subscriber::{
    EnvFilter,
    field::RecordFields,
    fmt::{FormatFields, format::Writer},
};

use common::{Fixture, phases};

/// Initializes the tracing subscriber with one of two formats:
///
/// - **Full format** (when running under CI, or when
///   `INTEGRATION_TEST_FULL_LOG` is set): timestamp, level, target, message,
///   and structured fields. Useful for CI log archives and JSON parsing.
/// - **Minimal format** (the default for local runs): just the formatted
///   message text. The Given-When-Then DSL's message already encodes step
///   kind and name (e.g. `  GIVEN no party at this prefix yet`); timestamps,
///   targets, levels, and structured fields are stripped so the trace reads
///   like the spec sample in #56.
///
/// `try_init` is used so reruns in the same process don't double-init.
fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let full_log =
        std::env::var("CI").is_ok() || std::env::var("INTEGRATION_TEST_FULL_LOG").is_ok();

    if full_log {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_level(false)
            .without_time()
            .fmt_fields(MessageOnlyFields)
            .try_init();
    }
}

/// Field formatter that emits only the `message` field. Used by the minimal
/// (local) format to suppress structured fields like `step_kind=Given`,
/// `step_name=...`, `scenario=...` that the DSL attaches for JSON-formatter
/// consumption in CI.
struct MessageOnlyFields;

impl<'w> FormatFields<'w> for MessageOnlyFields {
    fn format_fields<R: RecordFields>(
        &self,
        mut writer: Writer<'w>,
        fields: R,
    ) -> std::fmt::Result {
        let mut visitor = MessageOnlyVisitor {
            writer: &mut writer,
        };
        fields.record(&mut visitor);
        Ok(())
    }
}

struct MessageOnlyVisitor<'a, 'w> {
    writer: &'a mut Writer<'w>,
}

impl<'a, 'w> Visit for MessageOnlyVisitor<'a, 'w> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            let _ = write!(self.writer, "{value}");
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.writer, "{value:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires running localnet — invoke via integration-tests/run.sh"]
async fn governance_workflows_e2e() -> anyhow::Result<()> {
    init_tracing();

    let mut f = Fixture::from_env()?;
    f.discover_network_parties().await?;
    // Regression guard for the subset-party hang: onboard a party with a
    // single invited peer (1 coordinator + 1 regular peer) before the
    // full-mesh happy path, which can't catch it because it invites everyone.
    phases::two_member_party::run(&mut f).await?;
    phases::create_dec_party::run(&mut f).await?;
    phases::distribute_dars::run(&mut f).await?;
    phases::check_peer_dars::run(&mut f).await?;
    phases::deploy_gov_core::run(&mut f).await?;
    phases::token_custody::run(&mut f).await?;
    phases::utility_onboarding::run(&mut f).await?;
    phases::generic_vote::run(&mut f).await?;
    phases::notification_feed::run(&mut f).await?;
    phases::owner_key_resilience::run(&mut f).await?;
    phases::kick::run(&mut f).await?;
    // Add-party edge cases must run while P3 is still OUT of the party
    // (validation 400s/409s, decline cascade, cancel cascade) — both
    // teardown paths leave P3 un-added for the happy-path phase below.
    phases::add_party_edge_cases::run(&mut f).await?;
    // Re-add the member kick just removed: full add flow (P3 key generation,
    // threshold signatures, topology growth, ACS sync, onboarding-flag
    // clearing) + the already-member and same-party 409 guards.
    phases::add_party::run(&mut f).await?;

    // ----------------------------------------------------------------------
    // Negative-path / chaos tests. They mutate workflow_runs (cancel/dismiss)
    // and intentionally kill+restart dec-party-manager processes, so they
    // run after the happy-path flow. Each generates its own fresh prefix and
    // dismisses the rows it creates. PIDs are tracked in `f.current_pids` so
    // restarts compose across phases.
    // ----------------------------------------------------------------------
    // Peer-health flip (kill a peer, assert P1 reports it Unreachable, restart,
    // assert Connected again). Runs first in the chaos block, where the mesh is
    // known healthy; it respawns P2 before returning so later phases see all
    // three nodes.
    phases::peer_health_flip::run(&mut f).await?;
    phases::identity_survives_dismiss::run(&mut f).await?;
    phases::cancel_cascades::run(&mut f).await?;
    phases::start_handler_conflict_409::run(&mut f).await?;
    phases::restart_coordinator_resume::run(&mut f).await?; // G1
    phases::restart_peer_resume::run(&mut f).await?; // G2
    phases::retry_coordinator_broadcast::run(&mut f).await?; // G3
    phases::dismiss_failed_cleans_artifacts::run(&mut f).await?; // G4
    phases::generate_keys_idempotent::run(&mut f).await?; // G7
    // G8 (peer 3-strikes abort) is intentionally NOT run. The
    // `peer_3_strikes_abort` phase is an unimplemented stub: exercising it
    // needs a raw-Noise-frame injection harness to feed a peer three
    // undeserializable payloads, which doesn't exist yet. Previously it was
    // called here and returned Ok(()) without asserting anything — a phase
    // that always "passed", i.e. misleading coverage — so it's left out of the
    // sequence until the harness lands. (Tracked by the TODO in that module.)
    // G9: restart with two concurrent kinds in flight — under the registry
    // model the recovery path must resume BOTH InProgress coordinator rows
    // (the old single-slot model resumed only the newest). Its original
    // disable reason (the shared per-kind dars_state singleton) no longer
    // exists.
    phases::restart_with_concurrent_kinds::run(&mut f).await?; // G9
    phases::retry_with_offline_peer::run(&mut f).await?; // P2
    // G12: cancelling one of two concurrent sibling runs must leave the
    // other's invites and peer rows untouched (instance-scoped CancelInvite +
    // per-instance cancel endpoint).
    phases::concurrent_sibling_cancel::run(&mut f).await?; // G12
    // G13: a delivered invitation must survive a peer restart (persisted +
    // reloaded at boot) and still be acceptable afterwards.
    phases::invite_survives_peer_restart::run(&mut f).await?; // G13
    // G14: declining one of two sibling invites fails only that run — the
    // peer-side mirror of G12 (decline routing + instance-stamped teardown).
    phases::concurrent_sibling_decline::run(&mut f).await?; // G14
    // G15: pending invitations from one coordinator are capped at 16, oldest
    // evicted — the only bound on invite intake now that busy-gating is gone.
    phases::invite_cap::run(&mut f).await?; // G15
    // G11 runs LAST: it is the heaviest phase (six concurrent coordinator
    // runs, twelve peer runs) and the full-feature test of concurrent
    // multi-instance workflows — full-mesh cross-acceptance of simultaneous
    // Onboarding + DARs coordinators on every node.
    phases::concurrent_cross_workflows::run(&mut f).await?; // G11
    Ok(())
}
