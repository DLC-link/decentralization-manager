use std::path::Path;

use crate::{config::Config, error::Result};

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
pub async fn run_all_steps(
    config: &Config,
    dars_dir: &Path,
    keys_dir: &Path,
    ids_dir: &Path,
    out_dir: &Path,
) -> Result {
    tracing::info!("Starting full workflow");

    // Construct all directory paths once
    let step_2_dir = out_dir.join("step_2");
    let step_2a_dir = out_dir.join("step_2a");
    let step_2a_signed_dir = step_2a_dir.join("signed-proposals");
    let step_3_dir = out_dir.join("step_3");
    let step_3a_dir = out_dir.join("step_3a");
    let step_3a_signed_dir = step_3a_dir.join("signed-proposals");

    // Step 1: Upload DARs and generate keys (run on each participant)
    tracing::info!("Step 1: Upload DARs and generate keys");
    upload_dars(config, dars_dir).await?;
    generate_keys(config, keys_dir, ids_dir).await?;

    // Step 1a: Create proposals (run once by coordinator)
    tracing::info!("Step 1a: Create proposals");
    create_proposals(config, keys_dir, ids_dir, out_dir).await?;

    // Step 2: Sign DNS proposals (run on each attestor)
    tracing::info!("Step 2: Sign DNS proposals");
    sign_dns_proposals(config, &step_2_dir, &step_2a_signed_dir, ids_dir).await?;

    // Step 2a: Submit DNS proposals (run once by coordinator)
    tracing::info!("Step 2a: Submit DNS proposals");
    submit_dns_proposals(config, &step_2_dir, &step_2a_dir).await?;

    // Step 3: Sign P2P/PTK proposals (run on each attestor)
    tracing::info!("Step 3: Sign P2P/PTK proposals");
    sign_p2p_ptk_proposals(config, &step_3_dir, &step_3a_signed_dir, ids_dir).await?;

    // Step 3a: Submit final proposals (run once by coordinator)
    tracing::info!("Step 3a: Submit final proposals");
    submit_final_proposals(config, &step_3_dir, &step_3a_dir).await?;

    // Step 3b: Prepare submissions (run once by coordinator)
    tracing::info!("Step 3b: Prepare submissions");
    prepare_submissions(config, out_dir).await?;

    // Step 4: Sign submissions (run on each attestor)
    tracing::info!("Step 4: Sign submissions");
    sign_submissions().await?;

    // Step 5: Execute submissions (run once by coordinator)
    tracing::info!("Step 5: Execute submissions");
    execute_submissions().await?;

    tracing::info!("Full workflow completed successfully");
    Ok(())
}
