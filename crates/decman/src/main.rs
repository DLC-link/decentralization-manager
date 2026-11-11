mod cli;

use std::{path::PathBuf, process::ExitCode};

use dec_party_manager::{config::NodeConfig, db, error::Result, utils};
use tracing_subscriber::{filter::EnvFilter, fmt::MakeWriter, prelude::*, registry::LookupSpan};

use cli::{Cli, Commands, Parser};

/// Extract the --dir / -d value from raw args before clap parses,
/// so we can load the .env file from that directory first.
fn find_dir_arg() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        let arg = &args[i];
        if (arg == "-d" || arg == "--dir") && i + 1 < args.len() {
            return PathBuf::from(&args[i + 1]);
        }
        if let Some(dir) = arg.strip_prefix("--dir=") {
            return PathBuf::from(dir);
        }
        if let Some(dir) = arg.strip_prefix("-d")
            && !dir.is_empty()
        {
            return PathBuf::from(dir);
        }
    }
    PathBuf::from(".")
}

/// JSON is the default so each `tracing` field becomes a queryable attribute in
/// SigNoz. The console format stays available for a developer running the binary
/// by hand: set `DECPM_LOG_FORMAT=text`.
fn json_logs_enabled(format: Option<&str>) -> bool {
    !format.is_some_and(|f| f.trim().eq_ignore_ascii_case("text"))
}

/// The layer whose output the SigNoz log pipeline parses. `severity_parser`
/// reads `level` and `timestamp`, so both stay at the top level, and the event's
/// own fields nest under `fields`. `with_ansi(false)` drops the colouring, which
/// the layer applies even when stdout is not a terminal.
fn json_layer<S, W>(writer: W) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> MakeWriter<'a> + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_writer(writer)
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Returning the error from `main` prints it through `Termination`,
            // as plain text no pipeline parses. The error-rate alert counts
            // parsed `level` values, so the fatal has to leave through
            // `tracing` to be counted at all.
            tracing::error!(%error, "dec-party-manager exited with an error");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result {
    load_dotenv();
    init_logging();

    let args = Cli::parse();

    // Build config from defaults + CLI/env var overrides
    let mut config = NodeConfig::default().with_root_dir(&args.dir);

    args.command.init_db_encryption();
    args.command.apply_to(&mut config)?;

    // Resolve participant_id from Canton if not configured
    utils::resolve_participant_id(&mut config).await?;

    tracing::info!(
        "Running as participant: {participant_id}",
        participant_id = config.participant_id()
    );

    // Initialize database
    let db_path = match &args.command {
        Commands::Serve { db, .. } => db.clone().unwrap_or_else(|| config.db_path()),
    };
    tracing::info!(
        "Connecting to database at {db_path}",
        db_path = db_path.display()
    );
    let pool = db::connect(&db_path).await?;

    tracing::info!("Running database migrations");
    db::repair_migration_checksums(&pool).await?;
    db::MIGRATOR.run(&pool).await?;

    match args.command {
        Commands::Serve {
            ref host,
            port,
            ref admin_role,
            ref jwt_role_claim,
            ref allowed_origin,
            ..
        } => {
            dec_party_manager::server::start_server(
                host,
                port,
                config,
                pool,
                admin_role.clone(),
                jwt_role_claim.clone(),
                allowed_origin.clone(),
            )
            .await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}

/// Load .env from the root directory before clap parses, so DECPM_* env vars
/// are available for clap's env feature.
fn load_dotenv() {
    let dir = find_dir_arg();
    let env_path = dir.join(".env");
    if env_path.exists() {
        dotenvy::from_path(&env_path).ok();
    }
}

/// Log through `tracing`, as JSON unless `DECPM_LOG_FORMAT=text`, and log a
/// panic before the default hook prints it.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("dec_party_manager=info,tokio_noise=error,hyper_noise=error")
    });

    if json_logs_enabled(std::env::var("DECPM_LOG_FORMAT").ok().as_deref()) {
        tracing_subscriber::registry()
            .with(json_layer(std::io::stdout).with_filter(filter))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_filter(filter))
            .init();
    }

    // A panic prints to stderr as plain text, which the pipeline leaves
    // unparsed, so log it before the default hook prints the backtrace.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "dec-party-manager panicked");
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use anyhow::anyhow;
    use tracing_subscriber::{fmt::MakeWriter, prelude::*};

    use super::{json_layer, json_logs_enabled};

    /// A writer the test reads back once the layer has written to it.
    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut written = self
                .0
                .lock()
                .map_err(|_| io::Error::other("the buffer lock is poisoned"))?;
            written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedBuffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The dlc-infra log pipeline reads this exact shape. `severity_parser`
    /// takes `level` and `timestamp` from the top level, and `json_parser`
    /// flattens `fields` to the leaf key, so `count` arrives as a numeric
    /// attribute. Adding `.flatten_event(true)` breaks both, and the string
    /// tests below would still pass.
    #[test]
    fn the_json_line_carries_the_shape_the_pipeline_parses() -> anyhow::Result<()> {
        let buffer = SharedBuffer::default();
        let subscriber = tracing_subscriber::registry().with(json_layer(buffer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                count = 7,
                decparty = "cbtc-network::1220",
                "reassigned coupon batch"
            );
        });

        let written = buffer
            .0
            .lock()
            .map_err(|_| anyhow!("the buffer lock is poisoned"))?
            .clone();
        let line = String::from_utf8(written)?;
        assert!(
            !line.contains('\u{1b}'),
            "an ANSI escape survived into the line: {line}"
        );

        let event: serde_json::Value = serde_json::from_str(line.trim())?;
        assert_eq!(event["level"], "INFO");
        assert!(
            event["timestamp"].is_string(),
            "the severity parser needs a top-level timestamp: {event}"
        );
        assert_eq!(event["fields"]["message"], "reassigned coupon batch");
        assert_eq!(event["fields"]["decparty"], "cbtc-network::1220");
        assert_eq!(event["fields"]["count"], 7);
        assert!(
            event["fields"]["count"].is_number(),
            "count must stay numeric so an alert can compare it: {event}"
        );

        Ok(())
    }

    #[test]
    fn json_is_the_default_when_the_variable_is_unset() {
        assert!(json_logs_enabled(None));
    }

    #[test]
    fn text_selects_the_console_format() {
        assert!(!json_logs_enabled(Some("text")));
    }

    #[test]
    fn the_text_value_tolerates_case_and_padding() {
        assert!(!json_logs_enabled(Some(" TEXT ")));
    }

    #[test]
    fn any_other_value_stays_on_json() {
        assert!(json_logs_enabled(Some("json")));
        assert!(json_logs_enabled(Some("")));
        assert!(json_logs_enabled(Some("pretty")));
    }
}
