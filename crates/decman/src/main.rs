mod cli;

use std::{path::PathBuf, process::ExitCode};

use dec_party_manager::{
    config::{Auth0Config, CantonTlsConfig, KeycloakConfig, NodeConfig},
    db,
    error::Result,
    utils,
};
use tracing_subscriber::{filter::EnvFilter, prelude::*};

use cli::{Cli, Commands, Parser};

/// The TLS flags for one Canton endpoint, as parsed from the CLI/env.
struct TlsOverrides<'a> {
    enabled: &'a Option<bool>,
    ca_cert: &'a Option<String>,
    client_cert: &'a Option<String>,
    client_key: &'a Option<String>,
    domain: &'a Option<String>,
}

/// Apply whichever TLS flags the operator set, leaving the rest at their
/// defaults (TLS off, platform trust store, no client identity).
fn apply_tls_overrides(tls: &mut CantonTlsConfig, overrides: TlsOverrides<'_>) {
    if let Some(enabled) = overrides.enabled {
        tls.enabled = *enabled;
    }
    if let Some(path) = overrides.ca_cert {
        tls.ca_cert = Some(path.clone());
    }
    if let Some(path) = overrides.client_cert {
        tls.client_cert = Some(path.clone());
    }
    if let Some(path) = overrides.client_key {
        tls.client_key = Some(path.clone());
    }
    if let Some(domain) = overrides.domain {
        tls.domain = Some(domain.clone());
    }
}

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
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + 'static,
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
    // Load .env from the root directory before clap parses,
    // so DECPM_* env vars are available for clap's env feature
    let dir = find_dir_arg();
    let env_path = dir.join(".env");
    if env_path.exists() {
        dotenvy::from_path(&env_path).ok();
    }

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

    let args = Cli::parse();

    // Build config from defaults + CLI/env var overrides
    let mut config = NodeConfig::default().with_root_dir(&args.dir);

    match &args.command {
        Commands::Serve {
            listen_address,
            noise_port,
            public_address,
            canton_admin_host,
            canton_admin_port,
            canton_ledger_host,
            canton_ledger_port,
            canton_admin_tls,
            canton_admin_tls_ca_cert,
            canton_admin_tls_client_cert,
            canton_admin_tls_client_key,
            canton_admin_tls_domain,
            canton_ledger_tls,
            canton_ledger_tls_ca_cert,
            canton_ledger_tls_client_cert,
            canton_ledger_tls_client_key,
            canton_ledger_tls_domain,
            canton_synchronizer,
            canton_network,
            keycloak_url,
            keycloak_realm,
            keycloak_client_id,
            keycloak_internal_url,
            auth0_domain,
            auth0_client_id,
            auth0_audience,
            auth0_scope,
            jwt_role_claim: _,
            timeout_handshake,
            timeout_message,
            timeout_retry_attempts,
            timeout_retry_delay,
            noise_retry_timeout_sec,
            noise_retry_max_attempts,
            noise_retry_backoff_ms,
            reward_automation_interval_secs,
            reward_expiry_read_interval_secs,
            reward_max_creates,
            reward_min_expiry_margin_secs,
            metrics_port,
            db_encryption_key,
            insecure,
            canton_hmac_secret,
            canton_hmac_audience,
            canton_hmac_subject,
            tenant_api_keys,
            ..
        } => {
            if let Some(key) = db_encryption_key {
                dec_party_manager::db::crypto::init_key(key);
                tracing::info!("Database encryption enabled");
            }
            if let Some(addr) = listen_address {
                config.node.listen_address = addr.clone();
            }
            if let Some(p) = noise_port {
                config.node.port = *p;
            }
            if let Some(addr) = public_address {
                config.node.public_address = Some(addr.clone());
            }
            if let Some(host) = canton_admin_host {
                config.canton.admin_api_host = host.clone();
            }
            if let Some(p) = canton_admin_port {
                config.canton.admin_api_port = *p;
            }
            if let Some(host) = canton_ledger_host {
                config.canton.ledger_api_host = host.clone();
            }
            if let Some(p) = canton_ledger_port {
                config.canton.ledger_api_port = *p;
            }
            apply_tls_overrides(
                &mut config.canton.admin_api_tls,
                TlsOverrides {
                    enabled: canton_admin_tls,
                    ca_cert: canton_admin_tls_ca_cert,
                    client_cert: canton_admin_tls_client_cert,
                    client_key: canton_admin_tls_client_key,
                    domain: canton_admin_tls_domain,
                },
            );
            apply_tls_overrides(
                &mut config.canton.ledger_api_tls,
                TlsOverrides {
                    enabled: canton_ledger_tls,
                    ca_cert: canton_ledger_tls_ca_cert,
                    client_cert: canton_ledger_tls_client_cert,
                    client_key: canton_ledger_tls_client_key,
                    domain: canton_ledger_tls_domain,
                },
            );
            if let Some(sync) = canton_synchronizer {
                config.canton.synchronizer = sync.clone();
            }
            if let Some(net) = canton_network {
                config.canton.network = *net;
            }
            // `internal_url` is deliberately not in this guard: it only
            // supplements a real Keycloak config and is meaningless on its
            // own, so it must not by itself materialize a config with empty
            // url/realm/client_id. It is applied below only when one of those
            // three has already established the config.
            if keycloak_url.is_some() || keycloak_realm.is_some() || keycloak_client_id.is_some() {
                let kc = config.keycloak.get_or_insert(KeycloakConfig {
                    url: String::new(),
                    internal_url: None,
                    realm: String::new(),
                    client_id: String::new(),
                    client_secret: None,
                    username: None,
                    password: None,
                });
                if let Some(url) = keycloak_url {
                    kc.url = url.clone();
                }
                if let Some(internal_url) = keycloak_internal_url {
                    kc.internal_url = Some(internal_url.clone());
                }
                if let Some(realm) = keycloak_realm {
                    kc.realm = realm.clone();
                }
                if let Some(client_id) = keycloak_client_id {
                    kc.client_id = client_id.clone();
                }
            } else if keycloak_internal_url.is_some() {
                tracing::warn!(
                    "DECPM_KEYCLOAK_INTERNAL_URL is set but DECPM_KEYCLOAK_URL/REALM/CLIENT_ID \
                     are not; no Keycloak config was created and the internal URL is ignored"
                );
            }

            if let (Some(domain), Some(client_id)) = (auth0_domain, auth0_client_id) {
                config.auth0 = Some(Auth0Config {
                    domain: domain.clone(),
                    client_id: client_id.clone(),
                    audience: auth0_audience.clone(),
                    scope: auth0_scope.clone(),
                });
            } else if auth0_scope.is_some() {
                tracing::warn!(
                    "DECPM_AUTH0_SCOPE is set but DECPM_AUTH0_DOMAIN/CLIENT_ID are not; no \
                     Auth0 config was created and the scope is ignored"
                );
            }
            if let Some(v) = timeout_handshake {
                config.timeouts.handshake_timeout_secs = *v;
            }
            if let Some(v) = timeout_message {
                config.timeouts.message_timeout_secs = *v;
            }
            if let Some(v) = timeout_retry_attempts {
                config.timeouts.connection_retry_attempts = *v;
            }
            if let Some(v) = timeout_retry_delay {
                config.timeouts.connection_retry_delay_secs = *v;
            }
            if let Some(v) = noise_retry_timeout_sec {
                config.noise_retry.per_attempt_timeout_secs = *v;
            }
            if let Some(v) = noise_retry_max_attempts {
                config.noise_retry.max_attempts = *v;
            }
            if let Some(v) = noise_retry_backoff_ms {
                config.noise_retry.backoff_ms = *v;
            }
            if let Some(v) = reward_automation_interval_secs {
                config.reward_automation_interval_secs = *v;
            }
            if let Some(v) = reward_expiry_read_interval_secs {
                config.reward_expiry_read_interval_secs = *v;
            }
            if let Some(v) = reward_max_creates {
                config.reward_max_creates = *v;
            }
            if let Some(v) = reward_min_expiry_margin_secs {
                config.reward_min_expiry_margin_secs = *v;
            }
            if let Some(v) = metrics_port {
                config.metrics_port = *v;
            }

            config.tenant_api_keys = tenant_api_keys
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|k| !k.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();

            config.insecure = *insecure;
            // Only ingest the unsafe HMAC settings when insecure mode is on;
            // in a secure run they do nothing, so don't store them.
            if *insecure {
                if let Some(v) = canton_hmac_secret {
                    config.insecure_auth.secret = v.clone();
                }
                if let Some(v) = canton_hmac_audience {
                    config.insecure_auth.audience = v.clone();
                }
                if let Some(v) = canton_hmac_subject {
                    config.insecure_auth.subject = v.clone();
                }
            }
        }
    }

    // Resolve participant_id from Canton if not configured
    utils::resolve_participant_id(&mut config).await?;

    tracing::info!("Running as participant: {}", config.participant_id());

    // Initialize database
    let db_path = match &args.command {
        Commands::Serve { db, .. } => db.clone().unwrap_or_else(|| config.db_path()),
    };
    tracing::info!("Connecting to database at {}", db_path.display());
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;
    use tracing_subscriber::{fmt::MakeWriter, prelude::*};

    use super::{json_layer, json_logs_enabled};

    /// A writer the test reads back once the layer has written to it.
    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut written = self
                .0
                .lock()
                .map_err(|_| std::io::Error::other("the buffer lock is poisoned"))?;
            written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
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
