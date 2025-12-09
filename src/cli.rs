use std::path::PathBuf;

use clap::Subcommand;

pub use clap::Parser;

use dec_party_onboarding::participant_id::CantonId;

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

    /// Run the kick workflow (remove participant from decentralized party)
    Kick {
        /// Decentralized party ID to remove participant from
        #[arg(long, value_name = "PARTY_ID")]
        decentralized_party_id: CantonId,

        /// Participant ID to kick
        #[arg(long, value_name = "PARTICIPANT_ID")]
        participant_id: CantonId,

        /// Namespace fingerprint (DNS owner key) to remove
        #[arg(long, value_name = "NAMESPACE_FP")]
        namespace_fingerprint: String,
    },

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
