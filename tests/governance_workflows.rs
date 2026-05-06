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
    phases::create_dec_party::run(&mut f).await?;
    phases::distribute_dars::run(&mut f).await?;
    phases::check_peer_dars::run(&mut f).await?;
    phases::deploy_gov_core::run(&mut f).await?;
    phases::token_custody::run(&mut f).await?;
    phases::utility_onboarding::run(&mut f).await?;
    phases::generic_vote::run(&mut f).await?;
    phases::owner_key_resilience::run(&mut f).await?;
    phases::kick::run(&mut f).await?;
    Ok(())
}
