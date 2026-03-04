//! TCP listener accept loop — wraps moduvex-runtime's `TcpListener`.
//!
//! `Listener::bind` creates the TCP socket and registers it with the reactor.
//! `Listener::accept` returns a future that resolves to the next connection.

use std::io;
use std::net::SocketAddr;

use moduvex_runtime::net::{TcpListener, TcpStream};

/// Async TCP listener for the HTTP server.
pub struct Listener {
    inner: TcpListener,
    addr:  SocketAddr,
}

impl Listener {
    /// Bind to `addr` and start listening for TCP connections.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let inner = TcpListener::bind(addr)?;
        let addr  = inner.local_addr()?;
        Ok(Self { inner, addr })
    }

    /// The local address this listener is bound to.
    pub fn local_addr(&self) -> SocketAddr { self.addr }

    /// Accept the next incoming TCP connection.
    ///
    /// Returns `(TcpStream, peer_addr)` on success. Loops on transient errors
    /// (EAGAIN / ECONNABORTED) which can legitimately occur under load.
    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            match self.inner.accept().await {
                Ok(pair) => return Ok(pair),
                Err(e) if is_transient(&e) => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

/// Returns `true` for errors that should not terminate the accept loop.
fn is_transient(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_and_local_addr() {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .expect("bind failed");
        let addr = listener.local_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(addr.port() > 0);
    }
}
