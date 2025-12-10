use std::path::PathBuf;

use clap::Subcommand;

pub use clap::Parser;

#[derive(Parser)]
#[command(name = "dec-party-onboarding")]
#[command(about = "Canton decentralized party onboarding workflow automation", long_about = None)]
pub struct Cli {
    /// Path to configuration file (TOML)
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Generate Noise protocol static keypair for secure communication
    Keygen {
        /// Output file path for the private key
        #[arg(short, long, value_name = "FILE")]
        output: PathBuf,
    },

    /// Run the onboarding workflow (create decentralized party)
    Onboarding,

    /// Run the contracts workflow (upload DARs and create contracts)
    Contracts,

    /// Query decentralized parties from Canton topology
    QueryParties {
        /// Party ID prefix (e.g., "cbtc-network")
        #[arg(long, value_name = "PREFIX")]
        party_id_prefix: String,
    },

    /// Start the HTTP server for querying decentralized parties
    Serve {
        /// Host address to bind to
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        /// Port to listen on
        #[arg(long, default_value = "8080")]
        port: u16,
    },
}
