use crate::{config::NodeConfig, dirs::WorkflowDirs, error::Result};

// Step modules
pub mod step_1;
pub mod step_1a;
pub mod step_2;
pub mod step_2a;
pub mod step_3;
pub mod step_3a;
pub mod step_3b;
pub mod step_4;
pub mod step_5;

// Re-export step functions
pub use step_1::{generate_keys, upload_dars};
pub use step_1a::create_proposals;
pub use step_2::sign_dns_proposals;
pub use step_2a::submit_dns_proposals;
pub use step_3::sign_p2p_ptk_proposals;
pub use step_3a::submit_final_proposals;
pub use step_3b::prepare_submissions;
pub use step_4::sign_submissions;
pub use step_5::execute_submissions;

/// Run all steps in sequence
pub async fn run_all_steps(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Starting full workflow");

    // Step 1: Upload DARs and generate keys (run on each participant)
    tracing::info!("Step 1: Upload DARs and generate keys");
    upload_dars(config, dirs).await?;
    generate_keys(config, dirs).await?;

    // Step 1a: Create proposals (run once by coordinator)
    tracing::info!("Step 1a: Create proposals");
    create_proposals(config, dirs).await?;

    // Step 2: Sign DNS proposals (run on each attestor)
    tracing::info!("Step 2: Sign DNS proposals");
    sign_dns_proposals(config, dirs).await?;

    // Step 2a: Submit DNS proposals (run once by coordinator)
    tracing::info!("Step 2a: Submit DNS proposals");
    submit_dns_proposals(config, dirs).await?;

    // Step 3: Sign P2P proposals (run on each attestor)
    // Canton 3.4+: PTK deprecated, signing keys now in P2P mapping
    tracing::info!("Step 3: Sign P2P proposals");
    sign_p2p_ptk_proposals(config, dirs).await?;

    // Step 3a: Submit final proposals (run once by coordinator)
    tracing::info!("Step 3a: Submit final proposals");
    submit_final_proposals(config, dirs).await?;

    // Step 3b: Prepare submissions (run once by coordinator)
    tracing::info!("Step 3b: Prepare submissions");
    prepare_submissions(config, dirs).await?;

    // Step 4: Sign submissions (run on each attestor)
    tracing::info!("Step 4: Sign submissions");
    sign_submissions(config, dirs).await?;

    // Step 5: Execute submissions (run once by coordinator)
    tracing::info!("Step 5: Execute submissions");
    execute_submissions(config, dirs).await?;

    tracing::info!("Full workflow completed successfully");
    Ok(())
}
