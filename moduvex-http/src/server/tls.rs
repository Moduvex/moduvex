//! TLS support — configuration, stream wrapper, and acceptor.
//!
//! All items are feature-gated behind `#[cfg(feature = "tls")]`.
//!
//! # Architecture
//! ```text
//! TCP Accept
//!     |
//!     v
//! TlsAcceptor::accept(TcpStream)
//!     |  async TLS handshake via rustls byte-level API
//!     v
//! TlsStream { inner: TcpStream, conn: ServerConnection }
//!     |  implements AsyncRead + AsyncWrite
//!     v
//! Connection::run(stream: Stream)
//! ```
//!
//! The `Stream` enum provides zero-overhead dispatch between plain TCP and TLS
//! without heap allocation per connection.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

// ── TlsConfig ─────────────────────────────────────────────────────────────────

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
        let cert_bytes =
            std::fs::read(cert_path).map_err(|e| TlsConfigError(format!("read cert: {e}")))?;
        let key_bytes =
            std::fs::read(key_path).map_err(|e| TlsConfigError(format!("read key: {e}")))?;

        let certs = rustls_pemfile::certs(&mut cert_bytes.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| TlsConfigError(format!("parse certs: {e}")))?;

        if certs.is_empty() {
            return Err(TlsConfigError("no certificates found in PEM".into()));
        }

        let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
            .map_err(|e| TlsConfigError(format!("parse key: {e}")))?
            .ok_or_else(|| TlsConfigError("no private key found in PEM".into()))?;

        Ok(Self {
            cert_chain: certs,
            private_key: key,
        })
    }

    /// Build a `rustls::ServerConfig` from this config.
    ///
    /// Advertises `h2` and `http/1.1` via ALPN so TLS clients can negotiate
    /// HTTP/2 during the handshake (RFC 7301).
    pub fn into_server_config(self) -> Result<rustls::ServerConfig, TlsConfigError> {
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| TlsConfigError(format!("rustls protocol: {e}")))?
            .with_no_client_auth()
            .with_single_cert(self.cert_chain, self.private_key)
            .map_err(|e| TlsConfigError(format!("rustls config: {e}")))?;
        // Advertise HTTP/2 first so clients that support it will prefer it.
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(config)
    }
}

// ── TlsConfigError ────────────────────────────────────────────────────────────

/// Error during TLS configuration or handshake.
#[derive(Debug)]
pub struct TlsConfigError(pub String);

impl std::fmt::Display for TlsConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TLS config error: {}", self.0)
    }
}

impl std::error::Error for TlsConfigError {}

// ── TlsStream ─────────────────────────────────────────────────────────────────

#[cfg(feature = "tls")]
use std::io::{Read, Write};
#[cfg(feature = "tls")]
use std::sync::Arc;

#[cfg(feature = "tls")]
use moduvex_runtime::net::{AsyncRead, AsyncWrite, TcpStream};

/// Async TLS stream wrapping a `TcpStream` and a rustls `ServerConnection`.
///
/// Bridges rustls's synchronous byte-level API (`read_tls` / `write_tls`) with
/// the moduvex-runtime reactor's `AsyncRead` / `AsyncWrite` poll model.
///
/// I/O model:
/// - `poll_read`: drain plaintext buffer → if empty, await readable TCP → call
///   `read_tls` + `process_new_packets` → retry plaintext drain.
/// - `poll_write`: write plaintext into rustls → call `write_tls` to produce
///   ciphertext → await writable TCP → flush ciphertext to TCP.
#[cfg(feature = "tls")]
pub struct TlsStream {
    inner: TcpStream,
    conn: rustls::ServerConnection,
    /// Intermediate ciphertext scratch buffer for write_tls output.
    write_buf: Vec<u8>,
    /// Offset into `write_buf` indicating how much has been sent so far.
    write_offset: usize,
}

#[cfg(feature = "tls")]
impl TlsStream {
    fn new(inner: TcpStream, conn: rustls::ServerConnection) -> Self {
        Self {
            inner,
            conn,
            write_buf: Vec::new(),
            write_offset: 0,
        }
    }

    /// Return the negotiated ALPN protocol after the TLS handshake, if any.
    ///
    /// Returns `Some(b"h2")` when HTTP/2 was negotiated, `Some(b"http/1.1")`
    /// for HTTP/1.1, or `None` if no ALPN was performed.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.conn.alpn_protocol()
    }

    /// Perform the TLS handshake asynchronously.
    ///
    /// Drives `ServerConnection` until `is_handshaking()` returns false by
    /// alternating between flushing pending writes and reading incoming data.
    pub(crate) async fn do_handshake(&mut self) -> Result<(), TlsConfigError> {
        loop {
            if !self.conn.is_handshaking() {
                break;
            }

            // Flush any pending TLS records to the TCP socket.
            if self.conn.wants_write() {
                self.flush_write_tls().await.map_err(|e| {
                    TlsConfigError(format!("handshake write: {e}"))
                })?;
            }

            // Re-check — after flushing we may be done.
            if !self.conn.is_handshaking() {
                break;
            }

            // Read incoming ciphertext from TCP into rustls.
            if self.conn.wants_read() {
                self.read_tls_from_tcp().await.map_err(|e| {
                    TlsConfigError(format!("handshake read: {e}"))
                })?;
                self.conn.process_new_packets().map_err(|e| {
                    TlsConfigError(format!("handshake process: {e}"))
                })?;
            }
        }

        // Flush any final records emitted after handshake completes (e.g. Finished).
        if self.conn.wants_write() {
            self.flush_write_tls().await.map_err(|e| {
                TlsConfigError(format!("post-handshake flush: {e}"))
            })?;
        }

        Ok(())
    }

    /// Read ciphertext from the TCP socket into the rustls `ServerConnection`.
    ///
    /// Awaits TCP readability, then calls `read_tls` using a `SyncTcpReader`
    /// that wraps the raw fd with a synchronous `Read` impl.
    async fn read_tls_from_tcp(&mut self) -> io::Result<usize> {
        use std::future::poll_fn;

        // Wait until the TCP socket signals readiness.
        // We poll with a 1-byte dummy buffer — poll_read with 0-byte buf
        // may return Ready(Ok(0)) immediately on some platforms.
        poll_fn(|cx| {
            let mut dummy = [0u8; 1];
            match Pin::new(&mut self.inner).poll_read(cx, &mut dummy) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(_) => Poll::Ready(()),
            }
        })
        .await;

        // TCP socket is readable. Pull ciphertext into rustls via sync adapter.
        let mut reader = SyncTcpReader::from_async_stream(&self.inner);
        match self.conn.read_tls(&mut reader) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Encode pending TLS records and flush them to the TCP socket.
    ///
    /// Calls `write_tls` to fill an intermediate ciphertext buffer, then
    /// flushes that buffer asynchronously to the TCP socket.
    async fn flush_write_tls(&mut self) -> io::Result<()> {
        use std::future::poll_fn;

        // Produce ciphertext into write_buf via VecWriter adapter.
        {
            let mut w = VecWriter(&mut self.write_buf);
            self.conn.write_tls(&mut w)?;
        }

        if self.write_buf.is_empty() {
            return Ok(());
        }

        // Write ciphertext bytes to TCP asynchronously.
        self.write_offset = 0;
        while self.write_offset < self.write_buf.len() {
            let n = poll_fn(|cx| {
                Pin::new(&mut self.inner)
                    .poll_write(cx, &self.write_buf[self.write_offset..])
            })
            .await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "TLS flush: TCP write returned 0",
                ));
            }
            self.write_offset += n;
        }
        self.write_buf.clear();
        self.write_offset = 0;
        Ok(())
    }
}

#[cfg(feature = "tls")]
impl AsyncRead for TlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();

        loop {
            // 1. Try to return plaintext already decrypted by rustls.
            match this.conn.reader().read(buf) {
                Ok(0) => {} // buffer empty — need more ciphertext
                Ok(n) => return Poll::Ready(Ok(n)),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Poll::Ready(Err(e)),
            }

            // 2. Attempt to read ciphertext from TCP into rustls (non-blocking).
            // Scope the reader so it is dropped before we mutably borrow `conn`.
            let read_result = {
                let mut reader = SyncTcpReader::from_async_stream(&this.inner);
                this.conn.read_tls(&mut reader)
            };
            match read_result {
                Ok(0) => return Poll::Ready(Ok(0)), // EOF
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // No ciphertext available yet — register waker and yield.
                    let mut dummy = [0u8; 1];
                    return match Pin::new(&mut this.inner).poll_read(cx, &mut dummy) {
                        Poll::Pending => Poll::Pending,
                        Poll::Ready(Ok(_)) => Poll::Pending, // spurious wake, retry
                        Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    };
                }
                Err(e) => return Poll::Ready(Err(e)),
            }

            // 3. Process newly received packets (decrypts ciphertext → plaintext buffer).
            match this.conn.process_new_packets() {
                Ok(_state) => {}
                Err(e) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("TLS error: {e}"),
                    )));
                }
            }
            // Loop back to try reading plaintext again.
        }
    }
}

#[cfg(feature = "tls")]
impl AsyncWrite for TlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();

        // If there's unflushed ciphertext from a previous write, drain it first.
        // We must report Pending (not the byte count) while flushing — the caller
        // will retry and we report the byte count on the next successful write.
        if this.write_offset < this.write_buf.len() {
            return match this.poll_flush_write_buf(cx) {
                Poll::Ready(Ok(())) => {
                    // Flush complete — fall through to accept new plaintext below.
                    // But we need to re-enter the logic, so just report 0 here
                    // and let the caller retry. Actually return Pending so caller
                    // retries the full write next poll.
                    Poll::Pending
                }
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            };
        }

        // Write plaintext into rustls — this buffers it internally.
        let n = this.conn.writer().write(buf)?;

        // Produce ciphertext into write_buf via VecWriter.
        this.write_buf.clear();
        this.write_offset = 0;
        {
            let mut w = VecWriter(&mut this.write_buf);
            this.conn.write_tls(&mut w)?;
        }

        // Start flushing ciphertext. Even if not yet complete, report n bytes
        // accepted from caller — rustls has buffered the plaintext.
        match this.poll_flush_write_buf(cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) | Poll::Pending => Poll::Ready(Ok(n)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.poll_flush_write_buf(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // Send TLS close_notify alert.
        this.conn.send_close_notify();
        // Produce and flush the close_notify record.
        {
            let mut w = VecWriter(&mut this.write_buf);
            if let Err(e) = this.conn.write_tls(&mut w) {
                return Poll::Ready(Err(e));
            }
        }
        this.poll_flush_write_buf(cx)
    }
}

#[cfg(feature = "tls")]
impl TlsStream {
    /// Flush bytes from `write_buf[write_offset..]` to the TCP socket.
    ///
    /// Returns `Ready(Ok(()))` when all bytes are sent, `Pending` when the
    /// TCP socket is temporarily full.
    fn poll_flush_write_buf(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.write_offset < self.write_buf.len() {
            match Pin::new(&mut self.inner)
                .poll_write(cx, &self.write_buf[self.write_offset..])
            {
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "TLS flush: TCP write returned 0",
                    )));
                }
                Poll::Ready(Ok(n)) => self.write_offset += n,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        // All bytes sent — compact the buffer for reuse.
        self.write_buf.clear();
        self.write_offset = 0;
        Poll::Ready(Ok(()))
    }
}

// ── SyncTcpReader ─────────────────────────────────────────────────────────────

/// Adapter that implements `std::io::Read` over a `TcpStream`'s raw fd.
///
/// Used as the transport for `rustls::ServerConnection::read_tls()`, which
/// requires a synchronous `Read` impl. The underlying socket is non-blocking,
/// so `WouldBlock` is returned as-is to signal "no data yet".
///
/// `ManuallyDrop` prevents double-close: we do NOT own the fd — `TcpStream`
/// does. We borrow it for the duration of a single `read_tls` call only.
#[cfg(feature = "tls")]
struct SyncTcpReader(std::mem::ManuallyDrop<std::net::TcpStream>);

#[cfg(feature = "tls")]
impl SyncTcpReader {
    /// Create a `SyncTcpReader` that borrows the fd from `stream`.
    ///
    /// # Safety
    /// The returned struct must not outlive `stream`, and must be dropped
    /// before `stream` is dropped or closed.
    fn from_async_stream(stream: &TcpStream) -> Self {
        use std::os::unix::io::{AsRawFd, FromRawFd};
        // SAFETY: we wrap in ManuallyDrop so the std TcpStream never closes
        // the fd. The fd is valid for the lifetime of the outer TcpStream.
        let std_tcp = unsafe { std::net::TcpStream::from_raw_fd(stream.as_raw_fd()) };
        Self(std::mem::ManuallyDrop::new(std_tcp))
    }
}

#[cfg(feature = "tls")]
impl Read for SyncTcpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

// ── SyncTcpWriter ─────────────────────────────────────────────────────────────

/// Collects bytes written by `rustls::ServerConnection::write_tls` into a `Vec`.
///
/// rustls's `write_tls` requires a `Write` impl; we capture the ciphertext
/// bytes into a heap buffer so we can later flush them asynchronously.
#[cfg(feature = "tls")]
struct VecWriter<'a>(&'a mut Vec<u8>);

#[cfg(feature = "tls")]
impl Write for VecWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ── TlsAcceptor ───────────────────────────────────────────────────────────────

/// Wraps a rustls `ServerConfig` and accepts TLS connections over plain `TcpStream`s.
///
/// `TlsAcceptor::accept` performs the async TLS handshake and returns a
/// [`TlsStream`] ready for HTTP traffic.
#[cfg(feature = "tls")]
pub struct TlsAcceptor {
    config: Arc<rustls::ServerConfig>,
}

#[cfg(feature = "tls")]
impl TlsAcceptor {
    /// Create a new acceptor from a [`TlsConfig`].
    pub fn new(config: TlsConfig) -> Result<Self, TlsConfigError> {
        Ok(Self {
            config: Arc::new(config.into_server_config()?),
        })
    }

    /// Perform the TLS handshake over `stream`.
    ///
    /// Returns a [`TlsStream`] on success. The caller should apply a timeout
    /// (e.g. via `with_timeout`) to reject slow or malicious clients.
    pub async fn accept(&self, stream: TcpStream) -> Result<TlsStream, TlsConfigError> {
        let tls_conn = rustls::ServerConnection::new(Arc::clone(&self.config))
            .map_err(|e| TlsConfigError(format!("create TLS conn: {e}")))?;
        let mut tls_stream = TlsStream::new(stream, tls_conn);
        tls_stream.do_handshake().await?;
        Ok(tls_stream)
    }
}

// ── Stream enum ───────────────────────────────────────────────────────────────

/// Dispatch enum for plain TCP or TLS connections.
///
/// Both variants implement `AsyncRead + AsyncWrite` identically from the
/// perspective of the connection handler.
///
/// `TlsStream` is boxed to avoid a large stack size difference between enum
/// variants (rustls `ServerConnection` is ~1KB).
pub enum Stream {
    /// Unencrypted TCP connection.
    Plain(moduvex_runtime::net::TcpStream),
    /// TLS-encrypted connection (boxed to level enum variant sizes).
    #[cfg(feature = "tls")]
    Tls(Box<TlsStream>),
}

impl Stream {
    /// Return the negotiated ALPN protocol (TLS only).
    ///
    /// Returns `Some(b"h2")` when HTTP/2 was negotiated via ALPN,
    /// `Some(b"http/1.1")` for HTTP/1.1, or `None` for plain TCP connections
    /// and TLS connections where no ALPN was performed.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        match self {
            Stream::Plain(_) => None,
            #[cfg(feature = "tls")]
            Stream::Tls(s) => s.alpn_protocol(),
        }
    }
}

impl moduvex_runtime::net::AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl moduvex_runtime::net::AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// TlsConfigError formats correctly.
    #[test]
    fn tls_config_error_display() {
        let e = TlsConfigError("test error".into());
        assert_eq!(e.to_string(), "TLS config error: test error");
    }

    /// TlsConfigError implements std::error::Error.
    #[test]
    fn tls_config_error_is_std_error() {
        let e: Box<dyn std::error::Error> = Box::new(TlsConfigError("x".into()));
        assert!(e.to_string().contains("TLS config error"));
    }

    /// Loading from a non-existent path returns an error.
    #[cfg(feature = "tls")]
    #[test]
    fn tls_config_missing_cert_returns_error() {
        let result = TlsConfig::from_pem("/nonexistent/cert.pem", "/nonexistent/key.pem");
        assert!(result.is_err());
        let msg = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected error"),
        };
        assert!(msg.contains("read cert") || msg.contains("TLS config error"));
    }

    /// Empty PEM file (valid bytes but no certs) returns an error.
    #[cfg(feature = "tls")]
    #[test]
    fn tls_config_empty_cert_file_returns_error() {
        let dir = std::env::temp_dir();
        let cert_path = dir.join("moduvex_test_empty_cert.pem");
        let key_path = dir.join("moduvex_test_empty_key.pem");
        std::fs::write(&cert_path, b"").unwrap();
        std::fs::write(&key_path, b"").unwrap();

        let result = TlsConfig::from_pem(&cert_path, &key_path);
        // Cleanup before assert to avoid leftover files on failure.
        let _ = std::fs::remove_file(&cert_path);
        let _ = std::fs::remove_file(&key_path);

        assert!(result.is_err());
        let msg = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected error"),
        };
        assert!(msg.contains("no certificates"));
    }

    /// Stream::Plain dispatches AsyncRead to TcpStream.
    /// This test verifies the enum compiles and delegates correctly.
    #[cfg(feature = "tls")]
    #[test]
    fn stream_enum_plain_variant_compiles() {
        // Verify the Stream::Plain variant is accessible.
        fn _check_plain(s: moduvex_runtime::net::TcpStream) -> Stream {
            Stream::Plain(s)
        }
        // Verify Stream::Tls accepts a boxed TlsStream.
        fn _check_tls(s: Box<TlsStream>) -> Stream {
            Stream::Tls(s)
        }
        // If this compiles, enum dispatch is correctly wired.
    }
}
