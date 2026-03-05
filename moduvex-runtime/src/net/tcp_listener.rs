//! Async `TcpListener` — non-blocking TCP accept loop.
//!
//! Wraps a raw `SOCK_STREAM` socket registered with the reactor.
//! `accept()` returns a future that resolves when the OS has a connection
//! ready, driving the reactor via the `IoSource` readable future.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::platform::sys::{set_nonblocking, Interest};
use crate::reactor::source::{next_token, IoSource};

use super::sockaddr::{reclaim_raw_sockaddr, sockaddr_to_socketaddr, socketaddr_to_raw};
use super::TcpStream;

// ── TcpListener ───────────────────────────────────────────────────────────────

/// Async TCP listener that accepts incoming connections.
pub struct TcpListener {
    source: IoSource,
}

impl TcpListener {
    /// Bind a TCP listener to `addr`.
    ///
    /// Creates a socket, sets `SO_REUSEADDR`, binds, listens (backlog 128),
    /// sets non-blocking mode, and registers with the reactor.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let fd = create_tcp_socket(addr)?;
        set_so_reuseaddr(fd)?;
        bind_socket(fd, addr)?;
        listen_socket(fd, 128)?;
        set_nonblocking(fd)?;
        let source = IoSource::new(fd, next_token(), Interest::READABLE)?;
        Ok(Self { source })
    }

    /// Return the local address this listener is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        raw_local_addr(self.source.raw())
    }

    /// Return a future that resolves to the next accepted `TcpStream`.
    pub fn accept(&self) -> AcceptFuture<'_> {
        AcceptFuture { listener: self }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let fd = self.source.raw();
        // IoSource Drop deregisters from the reactor; we close the fd here.
        // SAFETY: fd is a valid socket we own exclusively; Drop runs once.
        unsafe { libc::close(fd) };
    }
}

// ── AcceptFuture ──────────────────────────────────────────────────────────────

/// Future returned by [`TcpListener::accept`].
///
/// Polls `readable()` on the underlying `IoSource` until the OS reports a
/// connection is ready, then calls `libc::accept` to obtain the fd.
pub struct AcceptFuture<'a> {
    listener: &'a TcpListener,
}

impl<'a> Future for AcceptFuture<'a> {
    type Output = io::Result<(TcpStream, SocketAddr)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Try to accept immediately first (edge-triggered — may already be ready).
        match try_accept(self.listener.source.raw()) {
            Ok(Some(result)) => return Poll::Ready(Ok(result)),
            Ok(None) => {} // WouldBlock — fall through to register waker
            Err(e) => return Poll::Ready(Err(e)),
        }

        // Arm READABLE interest and store the waker. When a connection arrives
        // the reactor fires the waker and we get re-polled.
        match Pin::new(&mut self.listener.source.readable()).poll(cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
        }
    }
}

// ── Unix helpers ──────────────────────────────────────────────────────────────

/// Attempt a non-blocking `accept`. Returns:
/// - `Ok(Some(...))` — new connection obtained
/// - `Ok(None)`      — WouldBlock / EAGAIN
/// - `Err(...)`      — real OS error
fn try_accept(listener_fd: i32) -> io::Result<Option<(TcpStream, SocketAddr)>> {
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&addr) as libc::socklen_t;

    // SAFETY: `listener_fd` is a valid listening socket; `addr` is zeroed and
    // sized to hold both IPv4 and IPv6 sockaddr variants.
    let conn_fd = unsafe {
        libc::accept(
            listener_fd,
            &mut addr as *mut _ as *mut libc::sockaddr,
            &mut len,
        )
    };

    if conn_fd == -1 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(err);
    }

    set_nonblocking(conn_fd)?;
    let peer = sockaddr_to_socketaddr(&addr, len)?;
    let stream = TcpStream::from_raw_fd(conn_fd)?;
    Ok(Some((stream, peer)))
}

/// Create a TCP socket appropriate for `addr` (IPv4 or IPv6).
fn create_tcp_socket(addr: SocketAddr) -> io::Result<i32> {
    let family = match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    // SAFETY: documented syscall with valid AF_INET/AF_INET6 + SOCK_STREAM constants.
    let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Set `SO_REUSEADDR` on `fd` to allow immediate rebind after restart.
fn set_so_reuseaddr(fd: i32) -> io::Result<()> {
    let val: libc::c_int = 1;
    // SAFETY: `fd` is a valid socket; `val` is a 4-byte SOL_SOCKET option value.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &val as *const libc::c_int as *const libc::c_void,
            std::mem::size_of_val(&val) as libc::socklen_t,
        )
    };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Bind `fd` to `addr`.
fn bind_socket(fd: i32, addr: SocketAddr) -> io::Result<()> {
    let (sa_ptr, sa_len) = socketaddr_to_raw(addr);
    // SAFETY: `fd` is a valid unbound socket; `sa_ptr`/`sa_len` describe a
    // valid sockaddr of the correct family. The kernel copies the data.
    let rc = unsafe { libc::bind(fd, sa_ptr, sa_len) };
    // Reclaim the Box immediately after the syscall.
    // SAFETY: `sa_ptr` was produced by `socketaddr_to_raw` with matching `addr`.
    unsafe { reclaim_raw_sockaddr(sa_ptr, addr) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Call `listen` with `backlog` on `fd`.
fn listen_socket(fd: i32, backlog: i32) -> io::Result<()> {
    // SAFETY: `fd` is a valid bound TCP socket.
    let rc = unsafe { libc::listen(fd, backlog) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Query the local address of `fd` via `getsockname`.
fn raw_local_addr(fd: i32) -> io::Result<SocketAddr> {
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&addr) as libc::socklen_t;
    // SAFETY: `fd` is a valid bound socket; `addr` buffer is large enough for
    // either sockaddr_in or sockaddr_in6.
    let rc = unsafe { libc::getsockname(fd, &mut addr as *mut _ as *mut libc::sockaddr, &mut len) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    sockaddr_to_socketaddr(&addr, len)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_and_local_addr() {
        let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).expect("bind failed");
        let addr = listener.local_addr().expect("local_addr failed");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(addr.port() > 0, "assigned port must be non-zero");
    }

    #[test]
    fn bind_ipv6_loopback() {
        // Some CI environments may not have IPv6 — skip gracefully.
        match TcpListener::bind("[::1]:0".parse().unwrap()) {
            Ok(listener) => {
                let addr = listener.local_addr().expect("local_addr failed");
                assert_eq!(addr.ip().to_string(), "::1");
            }
            Err(e) if e.kind() == io::ErrorKind::AddrNotAvailable => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
