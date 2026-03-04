//! Async `TcpStream` — non-blocking bidirectional TCP byte stream.
//!
//! Implements [`AsyncRead`] and [`AsyncWrite`] using `libc::read` / `libc::write`.
//! The underlying fd is registered with the reactor; `readable()` / `writable()`
//! futures from `IoSource` are used to suspend until the OS signals readiness.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::platform::sys::{set_nonblocking, Interest};
use crate::reactor::source::{next_token, IoSource};

use super::{AsyncRead, AsyncWrite};

// ── TcpStream ─────────────────────────────────────────────────────────────────

/// Async TCP stream. Implements `AsyncRead` + `AsyncWrite`.
pub struct TcpStream {
    source: IoSource,
}

impl TcpStream {
    /// Connect to `addr` asynchronously.
    ///
    /// Creates a non-blocking socket and starts a `connect()` call. Returns a
    /// [`ConnectFuture`] that resolves once the TCP handshake completes.
    pub fn connect(addr: SocketAddr) -> ConnectFuture {
        ConnectFuture::new(addr)
    }

    /// Wrap an already-connected raw file descriptor in a `TcpStream`.
    ///
    /// `fd` must be a connected, non-blocking TCP socket.
    ///
    /// # Errors
    /// Returns `Err` if reactor registration fails.
    pub(crate) fn from_raw_fd(fd: i32) -> io::Result<Self> {
        // Register for both directions so we can arm either on demand.
        let source = IoSource::new(fd, next_token(), Interest::READABLE | Interest::WRITABLE)?;
        Ok(Self { source })
    }

    /// Return the peer address of the connection.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        peer_addr(self.source.raw())
    }

    /// Return the local address of the connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        local_addr(self.source.raw())
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let fd = self.source.raw();
        // IoSource Drop deregisters from reactor; close the fd here.
        // SAFETY: we own `fd` exclusively; it is valid until this drop runs.
        unsafe { libc::close(fd) };
    }
}

// ── AsyncRead ─────────────────────────────────────────────────────────────────

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let fd = self.source.raw();

        // Try the read immediately — may already have data in the kernel buffer.
        // SAFETY: `fd` is a valid non-blocking socket; `buf` is a valid slice.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            return Poll::Ready(Ok(n as usize));
        }
        if n == 0 {
            return Poll::Ready(Ok(0)); // EOF
        }

        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            return Poll::Ready(Err(err));
        }

        // No data yet — register waker and wait for READABLE event.
        match Pin::new(&mut self.source.readable()).poll(cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
        }
    }
}

// ── AsyncWrite ────────────────────────────────────────────────────────────────

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let fd = self.source.raw();

        // SAFETY: `fd` is a valid non-blocking socket; `buf` is a valid slice.
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n >= 0 {
            return Poll::Ready(Ok(n as usize));
        }

        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            return Poll::Ready(Err(err));
        }

        // Socket send buffer full — wait for WRITABLE event.
        match Pin::new(&mut self.source.writable()).poll(cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TCP sockets are kernel-buffered — flush is a no-op.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let fd = self.source.raw();
        // SAFETY: `fd` is a valid socket; SHUT_WR is a documented constant.
        let rc = unsafe { libc::shutdown(fd, libc::SHUT_WR) };
        if rc == -1 {
            Poll::Ready(Err(io::Error::last_os_error()))
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

// ── ConnectFuture ─────────────────────────────────────────────────────────────

/// Future returned by [`TcpStream::connect`].
///
/// Phase 1: creates the socket and calls `connect()` (returns EINPROGRESS).
/// Phase 2: stores waker in reactor registry; on WRITABLE event, checks SO_ERROR.
pub struct ConnectFuture {
    state: ConnectState,
}

enum ConnectState {
    /// Not yet started — stores the address for lazy socket creation.
    Init(SocketAddr),
    /// Socket created, connect() in progress; waiting for WRITABLE.
    /// `waker_armed` tracks whether we already registered the waker this poll.
    Connecting {
        fd: i32,
        token: usize,
        /// True after initial `register()` — stays true across polls.
        registered: bool,
    },
    /// Done (stream returned or error returned).
    Done,
}

impl ConnectFuture {
    fn new(addr: SocketAddr) -> Self {
        Self {
            state: ConnectState::Init(addr),
        }
    }
}

impl Future for ConnectFuture {
    type Output = io::Result<TcpStream>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match &mut self.state {
                ConnectState::Init(addr) => {
                    let addr = *addr;
                    match start_connect(addr) {
                        Err(e) => {
                            self.state = ConnectState::Done;
                            return Poll::Ready(Err(e));
                        }
                        Ok((fd, connected)) => {
                            if connected {
                                // Instant connect — wrap fd directly.
                                self.state = ConnectState::Done;
                                return Poll::Ready(TcpStream::from_raw_fd(fd));
                            }
                            // Register fd for WRITABLE in the reactor so we
                            // get woken when connect() completes.
                            let token = next_token();
                            if let Err(e) = crate::reactor::with_reactor(|r| {
                                r.register(fd, token, Interest::WRITABLE)
                            }) {
                                unsafe { libc::close(fd) };
                                self.state = ConnectState::Done;
                                return Poll::Ready(Err(e));
                            }
                            self.state = ConnectState::Connecting {
                                fd,
                                token,
                                registered: true,
                            };
                            // Fall through to Connecting arm.
                        }
                    }
                }

                ConnectState::Connecting { fd, token, .. } => {
                    let fd = *fd;
                    let token = *token;

                    // Store waker so reactor wakes us on WRITABLE.
                    crate::reactor::with_reactor_mut(|r| {
                        r.wakers.set_write_waker(token, cx.waker().clone());
                    });

                    // Check if connect completed (may have raced since last poll).
                    match get_so_error(fd) {
                        Err(e) => {
                            // Clean up reactor registration.
                            let _ = crate::reactor::with_reactor_mut(|r| {
                                r.deregister_with_token(fd, token)
                            });
                            self.state = ConnectState::Done;
                            return Poll::Ready(Err(e));
                        }
                        Ok(Some(os_err)) => {
                            let _ = crate::reactor::with_reactor_mut(|r| {
                                r.deregister_with_token(fd, token)
                            });
                            unsafe { libc::close(fd) };
                            self.state = ConnectState::Done;
                            return Poll::Ready(Err(io::Error::from_raw_os_error(os_err)));
                        }
                        Ok(None) => {
                            // SO_ERROR == 0 means connected. But we may be
                            // polled here before the WRITABLE event fires on
                            // the very first poll (connect still in progress).
                            // Distinguish by checking if the socket is writable NOW.
                            if is_writable_now(fd) {
                                // Connect complete — deregister old token, wrap fd.
                                let _ = crate::reactor::with_reactor_mut(|r| {
                                    r.deregister_with_token(fd, token)
                                });
                                self.state = ConnectState::Done;
                                return Poll::Ready(TcpStream::from_raw_fd(fd));
                            }
                            // Not writable yet — waker stored above, wait.
                            return Poll::Pending;
                        }
                    }
                }

                ConnectState::Done => {
                    return Poll::Ready(Err(io::Error::other(
                        "ConnectFuture polled after completion",
                    )));
                }
            }
        }
    }
}

impl Drop for ConnectFuture {
    fn drop(&mut self) {
        if let ConnectState::Connecting { fd, token, .. } = self.state {
            // Clean up reactor and close fd if the future is dropped mid-connect.
            let _ = crate::reactor::with_reactor_mut(|r| r.deregister_with_token(fd, token));
            // SAFETY: fd is a valid socket we own; future is being dropped.
            unsafe { libc::close(fd) };
        }
    }
}

/// Non-blocking poll: returns true if `fd` is writable right now.
///
/// Uses `select` with a zero timeout to probe write-readiness.
fn is_writable_now(fd: i32) -> bool {
    // SAFETY: all libc types are initialized; select is a documented syscall.
    unsafe {
        let mut write_set: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut write_set);
        libc::FD_SET(fd, &mut write_set);
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let n = libc::select(
            fd + 1,
            std::ptr::null_mut(),
            &mut write_set,
            std::ptr::null_mut(),
            &mut tv,
        );
        n > 0 && libc::FD_ISSET(fd, &write_set)
    }
}

// ── Unix helpers ──────────────────────────────────────────────────────────────

/// Create a non-blocking TCP socket and call `connect()`.
///
/// Returns `(fd, connected)` where `connected` is `true` if the connection
/// completed immediately (rare, e.g. loopback).
fn start_connect(addr: SocketAddr) -> io::Result<(i32, bool)> {
    let family = match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    // SAFETY: documented syscall with valid AF_INET/AF_INET6 constants.
    let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    set_nonblocking(fd)?;

    let (sa, sa_len) = socketaddr_to_raw(addr);
    // SAFETY: `fd` is a valid socket; `sa`/`sa_len` describe a valid sockaddr.
    let rc = unsafe { libc::connect(fd, sa, sa_len) };
    // SAFETY: we used Box::into_raw in socketaddr_to_raw; reclaim the Box now.
    unsafe { reclaim_raw_sockaddr(sa, addr) };

    if rc == 0 {
        return Ok((fd, true)); // instant connect
    }

    let err = io::Error::last_os_error();
    // EINPROGRESS (or EAGAIN on some platforms) means "in progress" — normal.
    if err.raw_os_error() == Some(libc::EINPROGRESS) {
        return Ok((fd, false));
    }

    // Real error — close and propagate.
    unsafe { libc::close(fd) };
    Err(err)
}

/// Read `SO_ERROR` on `fd` to check connect completion status.
///
/// Returns `Ok(None)` on success, `Ok(Some(errno))` on connect failure,
/// `Err(...)` if getsockopt itself fails.
fn get_so_error(fd: i32) -> io::Result<Option<i32>> {
    let mut val: libc::c_int = 0;
    let mut len = std::mem::size_of_val(&val) as libc::socklen_t;
    // SAFETY: `fd` is a valid socket; `val`/`len` are correctly sized.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut val as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(if val == 0 { None } else { Some(val) })
}

/// Query the peer address of `fd`.
fn peer_addr(fd: i32) -> io::Result<SocketAddr> {
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&addr) as libc::socklen_t;
    // SAFETY: `fd` is a valid connected socket; `addr` is large enough.
    let rc = unsafe { libc::getpeername(fd, &mut addr as *mut _ as *mut libc::sockaddr, &mut len) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    sockaddr_to_socketaddr(&addr, len)
}

/// Query the local address of `fd`.
fn local_addr(fd: i32) -> io::Result<SocketAddr> {
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&addr) as libc::socklen_t;
    // SAFETY: `fd` is a valid socket; `addr` is large enough.
    let rc = unsafe { libc::getsockname(fd, &mut addr as *mut _ as *mut libc::sockaddr, &mut len) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    sockaddr_to_socketaddr(&addr, len)
}

/// Convert `SocketAddr` to a heap-allocated raw sockaddr pointer.
///
/// Caller must call `reclaim_raw_sockaddr` after the syscall to free memory.
fn socketaddr_to_raw(addr: SocketAddr) -> (*const libc::sockaddr, libc::socklen_t) {
    match addr {
        SocketAddr::V4(v4) => {
            let octets = v4.ip().octets();
            // SAFETY: zeroed() gives a valid bit pattern; we fill every field.
            let mut sin: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            sin.sin_family = libc::AF_INET as libc::sa_family_t;
            sin.sin_port = v4.port().to_be();
            sin.sin_addr = libc::in_addr {
                s_addr: u32::from_be_bytes(octets).to_be(),
            };
            let boxed = Box::new(sin);
            let ptr = Box::into_raw(boxed) as *const libc::sockaddr;
            let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            (ptr, len)
        }
        SocketAddr::V6(v6) => {
            // SAFETY: zeroed() gives a valid bit pattern; we fill every field.
            let mut sin6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = v6.port().to_be();
            sin6.sin6_flowinfo = v6.flowinfo();
            sin6.sin6_addr = libc::in6_addr {
                s6_addr: v6.ip().octets(),
            };
            sin6.sin6_scope_id = v6.scope_id();
            let boxed = Box::new(sin6);
            let ptr = Box::into_raw(boxed) as *const libc::sockaddr;
            let len = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
            (ptr, len)
        }
    }
}

/// # Safety
/// `ptr` must have been produced by `socketaddr_to_raw` with matching `addr`.
unsafe fn reclaim_raw_sockaddr(ptr: *const libc::sockaddr, addr: SocketAddr) {
    match addr {
        SocketAddr::V4(_) => drop(Box::from_raw(ptr as *mut libc::sockaddr_in)),
        SocketAddr::V6(_) => drop(Box::from_raw(ptr as *mut libc::sockaddr_in6)),
    }
}

/// Convert a kernel-filled `sockaddr_in6` buffer (may be `sockaddr_in`) to `SocketAddr`.
fn sockaddr_to_socketaddr(
    addr: &libc::sockaddr_in6,
    len: libc::socklen_t,
) -> io::Result<SocketAddr> {
    let family = addr.sin6_family as libc::c_int;
    match family {
        libc::AF_INET if len >= std::mem::size_of::<libc::sockaddr_in>() as u32 => {
            // SAFETY: kernel wrote AF_INET data of correct size.
            let v4: &libc::sockaddr_in =
                unsafe { &*(addr as *const _ as *const libc::sockaddr_in) };
            let ip = std::net::Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr));
            let port = u16::from_be(v4.sin_port);
            Ok(SocketAddr::V4(std::net::SocketAddrV4::new(ip, port)))
        }
        libc::AF_INET6 if len >= std::mem::size_of::<libc::sockaddr_in6>() as u32 => {
            let ip = std::net::Ipv6Addr::from(addr.sin6_addr.s6_addr);
            let port = u16::from_be(addr.sin6_port);
            Ok(SocketAddr::V6(std::net::SocketAddrV6::new(
                ip,
                port,
                addr.sin6_flowinfo,
                addr.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported address family: {family}"),
        )),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::block_on_with_spawn;
    use crate::net::TcpListener;

    /// Poll-based async read: keeps polling until `n` bytes are gathered.
    async fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) {
        use std::future::poll_fn;
        let mut filled = 0;
        while filled < buf.len() {
            let n = poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, &mut buf[filled..]))
                .await
                .expect("read_exact: io error");
            if n == 0 {
                break;
            } // EOF
            filled += n;
        }
    }

    /// Poll-based async write: keeps polling until all bytes are sent.
    async fn write_all(stream: &mut TcpStream, buf: &[u8]) {
        use std::future::poll_fn;
        let mut sent = 0;
        while sent < buf.len() {
            let n = poll_fn(|cx| Pin::new(&mut *stream).poll_write(cx, &buf[sent..]))
                .await
                .expect("write_all: io error");
            sent += n;
        }
    }

    #[test]
    fn tcp_connect_and_echo() {
        block_on_with_spawn(async {
            // Bind a listener on a random port.
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            // Spawn a server task that accepts one connection and reads 5 bytes.
            let server = crate::spawn(async move {
                let (mut stream, _peer) = listener.accept().await.unwrap();
                let mut buf = [0u8; 5];
                read_exact(&mut stream, &mut buf).await;
                buf
            });

            // Connect as client and send "hello".
            let mut client = TcpStream::connect(addr).await.unwrap();
            write_all(&mut client, b"hello").await;

            // Shutdown write side so server's read returns EOF after 5 bytes.
            use std::future::poll_fn;
            poll_fn(|cx| Pin::new(&mut client).poll_shutdown(cx))
                .await
                .expect("shutdown failed");

            let received = server.await.unwrap();
            assert_eq!(&received, b"hello");
        });
    }
}
