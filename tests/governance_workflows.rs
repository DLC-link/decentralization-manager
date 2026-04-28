mod common;

use common::Fixture;
use common::phases;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires running localnet — invoke via integration-tests/run.sh"]
async fn governance_workflows_e2e() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let mut f = Fixture::from_env()?;
    phases::create_dec_party::run(&mut f).await?;
    phases::distribute_dars::run(&mut f).await?;
    phases::deploy_gov_core::run(&mut f).await?;
    phases::token_custody::run(&mut f).await?;
    phases::utility_onboarding::run(&mut f).await?;
    phases::generic_vote::run(&mut f).await?;
    Ok(())
}
