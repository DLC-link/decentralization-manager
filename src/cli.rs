use std::path::PathBuf;

use clap::Subcommand;

pub use clap::Parser;

#[derive(Parser)]
#[command(name = "grpc-test")]
#[command(about = "Canton workflow automation - port of Scala scripts to Rust", long_about = None)]
pub struct Cli {
    /// Path to configuration file (TOML)
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run all steps in sequence
    All,

    /// Generate Noise protocol static keypair for secure communication
    Keygen {
        /// Output file path for the private key
        #[arg(short, long, value_name = "FILE")]
        output: PathBuf,
    },

    /// Start the node (as coordinator or attestor based on configuration)
    Start,

    /// Step 1: Upload DARs
    UploadDars,

    /// Step 1: Generate keys and export participant ID
    GenerateKeys,

    /// Step 1a: Create topology proposals
    CreateProposals,

    /// Step 2: Sign DNS proposals
    SignDnsProposals,

    /// Step 2a: Submit DNS proposals
    SubmitDnsProposals,

    /// Step 3: Sign P2P and PTK proposals
    SignP2pPtkProposals,

    /// Step 3a: Submit final proposals
    SubmitFinalProposals,

    /// Step 3b: Prepare ledger submissions
    PrepareSubmissions,

    /// Step 4: Sign ledger submissions
    SignSubmissions,

    /// Step 5: Execute ledger submissions
    ExecuteSubmissions,
}
