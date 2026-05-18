mod common;

use tracing::field::{Field, Visit};
use tracing_subscriber::{
    EnvFilter,
    field::RecordFields,
    fmt::{FormatFields, format::Writer},
};

use common::{Fixture, TestTarget, phases};

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

    // ----------------------------------------------------------------------
    // Negative-path / chaos tests. They mutate workflow_runs (cancel/dismiss)
    // and intentionally kill+restart dec-party-manager processes, so they
    // run after the happy-path flow. Each generates its own fresh prefix and
    // dismisses the rows it creates. PIDs are tracked in `f.current_pids` so
    // restarts compose across phases.
    // ----------------------------------------------------------------------
    phases::identity_survives_dismiss::run(&mut f).await?;
    phases::cancel_cascades::run(&mut f).await?;
    phases::start_handler_conflict_409::run(&mut f).await?;

    // Phases G1 onward are gated to localnet until they've been validated on
    // devnet one at a time. G1 (`restart_coordinator_resume`) was observed
    // to time out at the 240s `poll_until` deadline on a devnet run against
    // post-#158 tip a1b29f0; the rest of the family (G2–G8, P1, P2) has
    // never been exercised on devnet. Tracked as a follow-up; the gate is
    // a one-line lift once each phase is signed off on devnet.
    if matches!(f.target, TestTarget::Localnet) {
        phases::restart_coordinator_resume::run(&mut f).await?; // G1
        phases::restart_peer_resume::run(&mut f).await?; // G2
        phases::retry_coordinator_broadcast::run(&mut f).await?; // G3
        phases::dismiss_failed_cleans_artifacts::run(&mut f).await?; // G4
        phases::generate_keys_idempotent::run(&mut f).await?; // G7
        phases::peer_3_strikes_abort::run(&mut f).await?; // G8 (stub)
        // G9 disabled: the concurrent-kinds resume scenario flakes on the shared
        // dars_state across chaos phases — the peer-handler/abort-handle race
        // is fixed but G10's stalled /dars/distribute + downstream P1 respawns
        // can still leave the row in a state where a fresh /dars/distribute
        // 409s. Re-enable once we've drained the pre-G9 dars_state more
        // aggressively (or moved G9 to its own fixture).
        // phases::restart_with_concurrent_kinds::run(&mut f).await?; // G9
        phases::failed_step_bounded_time::run(&mut f).await?; // P1
        phases::retry_with_offline_peer::run(&mut f).await?; // P2
    }
    Ok(())
}
