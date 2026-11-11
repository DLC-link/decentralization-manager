use anyhow::Context;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};

use super::CantonTlsConfig;
use crate::error::Result;

/// Build the tonic TLS settings for an endpoint.
///
/// With no `ca_cert` the platform trust store is used, which covers a
/// publicly-issued certificate; a participant behind a private CA needs the
/// CA PEM. `client_cert` + `client_key` are only for endpoints demanding
/// mTLS.
async fn client_tls_config(tls: &CantonTlsConfig, label: &str) -> Result<ClientTlsConfig> {
    let mut config = match &tls.ca_cert {
        Some(path) => {
            let pem = tokio::fs::read(path).await.with_context(|| {
                format!("reading the {label} API TLS CA certificate from {path}")
            })?;
            ClientTlsConfig::new().ca_certificate(Certificate::from_pem(pem))
        }
        None => ClientTlsConfig::new().with_enabled_roots(),
    };

    match (&tls.client_cert, &tls.client_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert = tokio::fs::read(cert_path).await.with_context(|| {
                format!("reading the {label} API TLS client certificate from {cert_path}")
            })?;
            let key = tokio::fs::read(key_path).await.with_context(|| {
                format!("reading the {label} API TLS client key from {key_path}")
            })?;
            config = config.identity(Identity::from_pem(cert, key));
        }
        (None, None) => {}
        (cert, _) => {
            let (set, missing) = if cert.is_some() {
                ("certificate", "key")
            } else {
                ("key", "certificate")
            };
            anyhow::bail!(
                "{label} API mTLS is half-configured: the client {set} is set but the \
                 client {missing} is not. Set both or neither."
            );
        }
    }

    if let Some(domain) = &tls.domain {
        config = config.domain_name(domain);
    }

    Ok(config)
}

/// How long a gRPC channel may take to establish before it is a failure.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the reachability probe in [`connect_advice`] waits.
const TCP_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a plain TCP connect to `uri` succeeds.
///
/// tonic reports a refused connection and a TLS mismatch through the same
/// opaque transport error, and reading its source chain only works while its
/// internals keep exposing one. A TCP connect is direct evidence instead: if
/// even that fails, nothing is listening and TLS cannot be the problem.
async fn tcp_reachable(uri: &tonic::transport::Uri) -> bool {
    let Some(host) = uri.host() else {
        return true;
    };
    // `Uri::host` keeps the brackets on an IPv6 literal, and `TcpStream`
    // resolves `[::1]` as a hostname and fails. Without this the advice claims
    // nothing is listening whenever the endpoint is addressed by IPv6.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
        Some("https") => 443,
        _ => 80,
    });

    matches!(
        tokio::time::timeout(
            TCP_PROBE_TIMEOUT,
            tokio::net::TcpStream::connect((host, port)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// What to tell the operator about a failed connect.
///
/// A TLS mismatch and an unreachable endpoint need opposite fixes, so the two
/// have to stay apart in the message. #260 was a real mismatch, but the same
/// hint on a refused connection points at the wrong knob: when nothing is
/// listening, no TLS setting changes the outcome.
fn connect_advice(reachable: bool, tls_enabled: bool, label: &str) -> String {
    if !reachable {
        return format!(
            "nothing accepts TCP connections there. The {label} API is down, \
             still starting up, or the host and port are wrong. TLS settings \
             play no part in this failure"
        );
    }

    let upper = label.to_uppercase();
    if tls_enabled {
        format!(
            "TLS is enabled for the {label} API. If the endpoint actually serves \
             plaintext h2c, unset DECPM_CANTON_{upper}_TLS; if it serves TLS under a \
             private CA, point DECPM_CANTON_{upper}_TLS_CA_CERT at that CA"
        )
    } else {
        format!(
            "the {label} API channel is plaintext. If the endpoint serves TLS it closes \
             the connection on the first bytes, which looks exactly like this; set \
             DECPM_CANTON_{upper}_TLS=true"
        )
    }
}

/// Connect a channel to `url`, applying `tls`.
///
/// The error path is the point of this function as much as the happy path:
/// a plaintext client against a TLS listener (and the reverse) fails deep in
/// the transport with nothing that names TLS, which is exactly how #260 was
/// reported — a permanent `transport error` / BrokenPipe on every call.
pub(super) async fn connect_channel(
    url: &str,
    tls: &CantonTlsConfig,
    label: &str,
) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .with_context(|| format!("{url} is not a valid {label} API endpoint"))?;

    // Bound connection establishment. Without this a black-holed address hangs
    // on the OS TCP timeout, which outlives any deadline a caller puts around
    // the call it is trying to make.
    endpoint = endpoint.connect_timeout(CONNECT_TIMEOUT);

    if tls.enabled {
        endpoint = endpoint
            .tls_config(client_tls_config(tls, label).await?)
            .with_context(|| format!("applying TLS settings to the {label} API channel"))?;
    }

    let error = match endpoint.connect().await {
        Ok(channel) => return Ok(channel),
        Err(error) => error,
    };

    let hint = connect_advice(tcp_reachable(endpoint.uri()).await, tls.enabled, label);
    Err(anyhow::Error::new(error).context(format!("connecting to {url}: {hint}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port nothing listens on, so `connect` fails immediately and the test
    /// only exercises how that failure is reported.
    const CLOSED: &str = "http://127.0.0.1:1";

    #[tokio::test]
    async fn half_configured_mtls_is_rejected() {
        let tls = CantonTlsConfig {
            enabled: true,
            client_cert: Some("/tmp/client.pem".to_string()),
            ..Default::default()
        };

        let message = match client_tls_config(&tls, "admin").await {
            Ok(_) => panic!("a client certificate without a key must be rejected"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            message.contains("half-configured"),
            "unhelpful error: {message}"
        );
    }

    #[tokio::test]
    async fn a_missing_ca_file_names_the_path() {
        let tls = CantonTlsConfig {
            enabled: true,
            ca_cert: Some("/nonexistent/ca.pem".to_string()),
            ..Default::default()
        };

        let message = match client_tls_config(&tls, "ledger").await {
            Ok(_) => panic!("an unreadable CA certificate must be rejected"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            message.contains("/nonexistent/ca.pem"),
            "unhelpful error: {message}"
        );
    }

    /// #260's reported symptom: a plaintext client against a TLS endpoint
    /// fails with a bare transport error naming nothing. The advice must point
    /// at the knob that fixes it. The server accepts the TCP connection and
    /// then drops it on the h2 preface, so the failure arrives as a reset or a
    /// broken pipe rather than as a connect error.
    #[test]
    fn a_plaintext_failure_points_at_the_tls_flag() {
        let advice = connect_advice(true, false, "admin");

        assert!(
            advice.contains("DECPM_CANTON_ADMIN_TLS=true"),
            "unhelpful advice: {advice}"
        );
    }

    /// And the mirror case, so an operator who turned TLS on against a
    /// plaintext endpoint is not left guessing either.
    #[test]
    fn a_tls_failure_points_back_at_plaintext_and_the_ca() {
        let advice = connect_advice(true, true, "ledger");

        assert!(
            advice.contains("unset DECPM_CANTON_LEDGER_TLS"),
            "unhelpful advice: {advice}"
        );
        assert!(
            advice.contains("DECPM_CANTON_LEDGER_TLS_CA_CERT"),
            "unhelpful advice: {advice}"
        );
    }

    /// An IPv6 endpoint that is listening must read as reachable. `Uri::host`
    /// keeps the brackets, and a bracketed literal handed to `TcpStream` is
    /// resolved as a hostname and fails, which would blame TLS for a healthy
    /// endpoint.
    #[tokio::test]
    async fn a_listening_ipv6_endpoint_is_reachable() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("[::1]:0").await?;
        let port = listener.local_addr()?.port();
        let uri: tonic::transport::Uri = format!("http://[::1]:{port}").parse()?;

        assert!(
            tcp_reachable(&uri).await,
            "a listening IPv6 endpoint must not read as unreachable"
        );
        Ok(())
    }

    /// The regression this pairs with: an unreachable endpoint has nothing to
    /// do with TLS, so saying otherwise points at the wrong knob. A refused
    /// connection must not name the flag in either direction.
    #[tokio::test]
    async fn a_refused_connection_does_not_blame_tls() {
        for (tls, label, flag) in [
            (
                CantonTlsConfig::default(),
                "admin",
                "DECPM_CANTON_ADMIN_TLS",
            ),
            (
                CantonTlsConfig {
                    enabled: true,
                    ..Default::default()
                },
                "ledger",
                "DECPM_CANTON_LEDGER_TLS",
            ),
        ] {
            let url = if tls.enabled {
                "https://127.0.0.1:1"
            } else {
                CLOSED
            };

            let message = match connect_channel(url, &tls, label).await {
                Ok(_) => panic!("nothing listens on port 1"),
                Err(e) => format!("{e:#}"),
            };
            assert!(
                !message.contains(flag),
                "a refused connection blamed TLS: {message}"
            );
            assert!(
                message.contains("nothing accepts TCP connections there"),
                "unhelpful error: {message}"
            );
        }
    }
}
