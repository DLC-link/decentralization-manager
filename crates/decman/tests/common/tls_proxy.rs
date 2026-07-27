//! A TLS-terminating TCP proxy, used to put a Canton API behind TLS.
//!
//! The localnet Splice stack serves its admin and ledger APIs as plaintext
//! h2c, so nothing in the e2e suite exercises the TLS path an operator gets
//! when their participant has TLS enabled. This proxy closes that gap without
//! touching the localnet bundle: it listens with a self-signed cert,
//! terminates TLS, and forwards the h2 byte stream to the real plaintext port.
//!
//! ALPN advertises `h2` because gRPC over TLS negotiates HTTP/2 there; the
//! upstream side then speaks the same h2 frames Canton expects with prior
//! knowledge.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, pki_types::PrivateKeyDer},
};

pub struct TlsProxy {
    /// Loopback port the proxy listens on; point the client here.
    pub port: u16,
    /// PEM of the self-signed cert, on disk. It is its own issuer, so the
    /// client trusts the endpoint by passing this as its CA.
    pub ca_cert_path: PathBuf,
    accept_loop: JoinHandle<()>,
}

impl TlsProxy {
    /// Listen on an ephemeral loopback port and forward to
    /// `127.0.0.1:upstream_port`. Writes the cert PEM into `cert_dir`.
    pub async fn start(upstream_port: u16, cert_dir: &Path) -> Result<Self> {
        // rustls needs a process-wide crypto provider. `install_default`
        // errors if one is already installed, which is fine — any provider
        // will do.
        let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();

        // The SANs cover both spellings a client might use for loopback; the
        // IP one is what matters, since DecMan is pointed at 127.0.0.1.
        let issued = rcgen::generate_simple_self_signed(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])
        .context("generating the self-signed proxy cert")?;

        let ca_cert_path = cert_dir.join(format!("canton-tls-proxy-{upstream_port}.pem"));
        tokio::fs::write(&ca_cert_path, issued.cert.pem())
            .await
            .with_context(|| format!("writing {}", ca_cert_path.display()))?;

        let key = PrivateKeyDer::try_from(issued.signing_key.serialize_der())
            .map_err(|e| anyhow::anyhow!("proxy key is not a valid private key: {e}"))?;
        let mut server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![issued.cert.der().clone()], key)
            .context("building the proxy TLS config")?;
        server_config.alpn_protocols = vec![b"h2".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .context("binding the TLS proxy listener")?;
        let port = listener.local_addr().context("proxy local_addr")?.port();

        let accept_loop = tokio::spawn(async move {
            loop {
                let Ok((downstream, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let mut tls = match acceptor.accept(downstream).await {
                        Ok(tls) => tls,
                        Err(e) => {
                            // A plaintext client hitting the TLS port lands
                            // here. That is the failure this proxy exists to
                            // surface, so make it visible in the test log.
                            tracing::warn!("TLS proxy handshake failed: {e}");
                            return;
                        }
                    };
                    let mut upstream =
                        match TcpStream::connect((Ipv4Addr::LOCALHOST, upstream_port)).await {
                            Ok(upstream) => upstream,
                            Err(e) => {
                                tracing::warn!("TLS proxy upstream connect failed: {e}");
                                return;
                            }
                        };
                    let _ = tokio::io::copy_bidirectional(&mut tls, &mut upstream).await;
                });
            }
        });

        tracing::info!("TLS proxy on 127.0.0.1:{port} -> 127.0.0.1:{upstream_port}");
        Ok(Self {
            port,
            ca_cert_path,
            accept_loop,
        })
    }
}

impl Drop for TlsProxy {
    fn drop(&mut self) {
        self.accept_loop.abort();
    }
}
