//! TLS configuration — load certificates and private keys from PEM files.
//!
//! The actual TLS stream wrapping (rustls I/O bridge) is deferred to a future
//! phase. This module establishes the config API so the `HttpServer` builder
//! can accept `.tls(TlsConfig::from_pem(...))`.

/// TLS configuration loaded from PEM-encoded certificate and key files.
///
/// # Example
/// ```ignore
/// let tls = TlsConfig::from_pem("cert.pem", "key.pem").unwrap();
/// HttpServer::bind("0.0.0.0:443").tls(tls).serve();
/// ```
#[cfg(feature = "tls")]
pub struct TlsConfig {
    pub(crate) cert_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    pub(crate) private_key: rustls::pki_types::PrivateKeyDer<'static>,
}

#[cfg(feature = "tls")]
impl TlsConfig {
    /// Load certificate chain and private key from PEM file paths.
    pub fn from_pem(
        cert_path: impl AsRef<std::path::Path>,
        key_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, TlsConfigError> {
        let cert_bytes = std::fs::read(cert_path)
            .map_err(|e| TlsConfigError(format!("read cert: {e}")))?;
        let key_bytes = std::fs::read(key_path)
            .map_err(|e| TlsConfigError(format!("read key: {e}")))?;

        let certs = rustls_pemfile::certs(&mut cert_bytes.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| TlsConfigError(format!("parse certs: {e}")))?;

        if certs.is_empty() {
            return Err(TlsConfigError("no certificates found in PEM".into()));
        }

        let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
            .map_err(|e| TlsConfigError(format!("parse key: {e}")))?
            .ok_or_else(|| TlsConfigError("no private key found in PEM".into()))?;

        Ok(Self { cert_chain: certs, private_key: key })
    }

    /// Build a `rustls::ServerConfig` from this config.
    pub fn into_server_config(self) -> Result<rustls::ServerConfig, TlsConfigError> {
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(self.cert_chain, self.private_key)
            .map_err(|e| TlsConfigError(format!("rustls config: {e}")))
    }
}

/// Error during TLS configuration.
#[derive(Debug)]
pub struct TlsConfigError(pub String);

impl std::fmt::Display for TlsConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TLS config error: {}", self.0)
    }
}

impl std::error::Error for TlsConfigError {}
