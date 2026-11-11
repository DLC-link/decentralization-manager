//! Copying the `serve` flags onto a [`NodeConfig`].

use anyhow::Context;
use dec_party_manager::{
    canton_id::CantonId,
    config::{Auth0Config, CantonTlsConfig, KeycloakConfig, NodeConfig},
    error::Result,
};

use super::Commands;

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

/// Split `DECPM_TENANT_API_KEYS` into keys, in the order configured. A repeat
/// keeps its first position, so the index an audit row records for a key
/// always names the same key.
fn parse_tenant_api_keys(raw: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in raw.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    keys
}

impl Commands {
    /// Turn on the encryption of database secrets when the operator set a
    /// key. Call it before [`Commands::apply_to`], as `run` always has.
    pub fn init_db_encryption(&self) {
        let Commands::Serve {
            db_encryption_key,
            key_store_key_file,
            ..
        } = self;
        if let Some(key) = db_encryption_key {
            dec_party_manager::db::crypto::init_key(key);
            tracing::info!("Database encryption enabled");
            if key_store_key_file.is_none() && !dec_party_manager::db::crypto::derives_keys(key) {
                tracing::warn!(
                    "DECPM_DB_ENCRYPTION_KEY has fewer than {} characters, so DecMan does \
                     not derive the key store's key from it",
                    dec_party_manager::db::crypto::MIN_DERIVING_PASSPHRASE_CHARS
                );
            }
        }
    }

    /// Copy every flag the operator set onto `config`, over its defaults.
    ///
    /// # Errors
    /// Returns an error if `DECPM_VALIDATOR_OPERATOR_PARTY` is not a valid
    /// party id.
    pub fn apply_to(&self, config: &mut NodeConfig) -> Result {
        let Commands::Serve {
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
            scan_url,
            keycloak_url,
            keycloak_realm,
            keycloak_client_id,
            keycloak_internal_url,
            auth0_domain,
            auth0_client_id,
            auth0_audience,
            auth0_scope,
            timeout_handshake,
            timeout_message,
            timeout_retry_attempts,
            timeout_retry_delay,
            noise_retry_timeout_sec,
            noise_retry_max_attempts,
            noise_retry_backoff_ms,
            reward_automation_interval_secs,
            reward_expiry_read_interval_secs,
            external_parties_ttl_secs,
            reward_max_creates,
            reward_min_expiry_margin_secs,
            metrics_port,
            insecure,
            canton_hmac_secret,
            canton_hmac_audience,
            canton_hmac_subject,
            tenant_api_keys,
            tenant_rate_limit_per_minute,
            validator_operator_party,
            key_store_key_file,
            ..
        } = self;
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
        config.canton.scan_url = scan_url.clone();
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
        if let Some(v) = external_parties_ttl_secs {
            config.external_parties_ttl_secs = *v;
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
            .map(parse_tenant_api_keys)
            .unwrap_or_default();
        if let Some(v) = tenant_rate_limit_per_minute {
            config.tenant_rate_limit_per_minute = *v;
        }
        config.validator_operator_party = validator_operator_party
            .as_deref()
            .map(str::trim)
            .filter(|party| !party.is_empty())
            .map(CantonId::parse)
            .transpose()
            .context("DECPM_VALIDATOR_OPERATOR_PARTY is not a valid party id")?;
        config.key_store_key_file = key_store_key_file.clone();

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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use dec_party_manager::config::NodeConfig;

    use super::*;
    use crate::cli::{Cli, Parser};

    /// The audit row names a tenant key by its index, so the index must follow
    /// the configured order and must not shift when a key is repeated.
    #[test]
    fn tenant_keys_keep_their_configured_order() {
        assert_eq!(
            parse_tenant_api_keys(" b , a,,b ,c"),
            vec!["b".to_string(), "a".to_string(), "c".to_string()]
        );
    }

    fn configured(flags: &[&str]) -> Result<NodeConfig> {
        let cli = Cli::try_parse_from(["dec-party-manager", "serve"].iter().chain(flags))?;
        let mut config = NodeConfig::default();
        cli.command.apply_to(&mut config)?;
        Ok(config)
    }

    #[test]
    fn serve_flags_override_the_defaults() -> Result<()> {
        let config = configured(&[
            "--listen-address",
            "127.0.0.1",
            "--noise-port",
            "7100",
            "--public-address",
            "node.example:7100",
            "--canton-admin-host",
            "admin.example",
            "--canton-admin-port",
            "5012",
            "--canton-ledger-host",
            "ledger.example",
            "--canton-ledger-port",
            "5011",
            "--canton-admin-tls",
            "true",
            "--canton-admin-tls-domain",
            "admin.tls.example",
            "--canton-ledger-tls-ca-cert",
            "/certs/ca.pem",
            "--canton-synchronizer",
            "sync-alias",
            "--scan-url",
            "https://scan.example",
            "--keycloak-url",
            "https://kc.example",
            "--keycloak-realm",
            "decman",
            "--keycloak-client-id",
            "decman-ui",
            "--keycloak-internal-url",
            "http://keycloak:8080",
            "--auth0-domain",
            "tenant.auth0.example",
            "--auth0-client-id",
            "auth0-client",
            "--timeout-handshake",
            "11",
            "--timeout-message",
            "12",
            "--timeout-retry-attempts",
            "13",
            "--timeout-retry-delay",
            "14",
            "--noise-retry-timeout-sec",
            "15",
            "--noise-retry-max-attempts",
            "16",
            "--noise-retry-backoff-ms",
            "17",
            "--reward-automation-interval-secs",
            "18",
            "--reward-expiry-read-interval-secs",
            "19",
            "--external-parties-ttl-secs",
            "20",
            "--reward-max-creates",
            "21",
            "--reward-min-expiry-margin-secs",
            "22",
            "--metrics-port",
            "0",
            "--tenant-api-keys",
            "b,a,b",
            "--tenant-rate-limit-per-minute",
            "23",
            "--key-store-key-file",
            "/keys/store.key",
            "--insecure",
            "--canton-hmac-secret",
            "hmac-secret",
            "--canton-hmac-audience",
            "hmac-audience",
            "--canton-hmac-subject",
            "hmac-subject",
        ])?;

        assert_eq!(config.node.listen_address, "127.0.0.1");
        assert_eq!(config.node.port, 7100);
        assert_eq!(
            config.node.public_address.as_deref(),
            Some("node.example:7100")
        );
        assert_eq!(config.canton.admin_api_host, "admin.example");
        assert_eq!(config.canton.admin_api_port, 5012);
        assert_eq!(config.canton.ledger_api_host, "ledger.example");
        assert_eq!(config.canton.ledger_api_port, 5011);
        assert!(config.canton.admin_api_tls.enabled);
        assert_eq!(
            config.canton.admin_api_tls.domain.as_deref(),
            Some("admin.tls.example")
        );
        assert_eq!(
            config.canton.ledger_api_tls.ca_cert.as_deref(),
            Some("/certs/ca.pem")
        );
        assert_eq!(config.canton.synchronizer, "sync-alias");
        assert_eq!(
            config.canton.scan_url.as_deref(),
            Some("https://scan.example")
        );

        let Some(keycloak) = &config.keycloak else {
            panic!("the Keycloak flags must create a Keycloak config");
        };
        assert_eq!(keycloak.url, "https://kc.example");
        assert_eq!(keycloak.realm, "decman");
        assert_eq!(keycloak.client_id, "decman-ui");
        assert_eq!(
            keycloak.internal_url.as_deref(),
            Some("http://keycloak:8080")
        );

        let Some(auth0) = &config.auth0 else {
            panic!("an Auth0 domain and client id must create an Auth0 config");
        };
        assert_eq!(auth0.domain, "tenant.auth0.example");
        assert_eq!(auth0.client_id, "auth0-client");

        assert_eq!(config.timeouts.handshake_timeout_secs, 11);
        assert_eq!(config.timeouts.message_timeout_secs, 12);
        assert_eq!(config.timeouts.connection_retry_attempts, 13);
        assert_eq!(config.timeouts.connection_retry_delay_secs, 14);
        assert_eq!(config.noise_retry.per_attempt_timeout_secs, 15);
        assert_eq!(config.noise_retry.max_attempts, 16);
        assert_eq!(config.noise_retry.backoff_ms, 17);
        assert_eq!(config.reward_automation_interval_secs, 18);
        assert_eq!(config.reward_expiry_read_interval_secs, 19);
        assert_eq!(config.external_parties_ttl_secs, 20);
        assert_eq!(config.reward_max_creates, 21);
        assert_eq!(config.reward_min_expiry_margin_secs, 22);
        assert_eq!(config.metrics_port, 0);
        assert_eq!(
            config.tenant_api_keys,
            vec!["b".to_string(), "a".to_string()]
        );
        assert_eq!(config.tenant_rate_limit_per_minute, 23);
        assert_eq!(
            config.key_store_key_file.as_deref(),
            Some(std::path::Path::new("/keys/store.key"))
        );
        assert!(config.insecure);
        assert_eq!(config.insecure_auth.secret, "hmac-secret");
        assert_eq!(config.insecure_auth.audience, "hmac-audience");
        assert_eq!(config.insecure_auth.subject, "hmac-subject");
        Ok(())
    }

    #[test]
    fn a_validator_operator_party_must_parse() {
        let Err(e) = configured(&["--validator-operator-party", "not a party"]) else {
            panic!("a malformed party id must be refused");
        };
        assert_eq!(
            e.to_string(),
            "DECPM_VALIDATOR_OPERATOR_PARTY is not a valid party id"
        );
    }
}
