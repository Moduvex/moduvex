//! Async `UdpSocket` — non-blocking UDP datagram socket.
//!
//! `send_to` / `recv_from` return futures that resolve when the OS is ready
//! to send or has data available, using the reactor's waker registry.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::platform::sys::{set_nonblocking, Interest};
use crate::reactor::source::{next_token, IoSource};

use super::sockaddr::{reclaim_raw_sockaddr, sockaddr_to_socketaddr, socketaddr_to_raw};

// ── UdpSocket ─────────────────────────────────────────────────────────────────

/// Async UDP datagram socket.
pub struct UdpSocket {
    source: IoSource,
}

impl UdpSocket {
    /// Bind a UDP socket to `addr`.
    ///
    /// Creates a `SOCK_DGRAM` socket, binds to `addr`, sets non-blocking, and
    /// registers with the reactor for both read and write readiness.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let fd = create_udp_socket(addr)?;
        bind_socket(fd, addr)?;
        set_nonblocking(fd)?;
        let source = IoSource::new(fd, next_token(), Interest::READABLE | Interest::WRITABLE)?;
        Ok(Self { source })
    }

    /// Return the local address the socket is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        raw_local_addr(self.source.raw())
    }

    /// Return a future that sends `buf` to `target` and resolves to the number
    /// of bytes sent.
    pub fn send_to<'a>(&'a self, buf: &'a [u8], target: SocketAddr) -> SendToFuture<'a> {
        SendToFuture {
            socket: self,
            buf,
            target,
        }
    }

    /// Return a future that receives a datagram into `buf` and resolves to
    /// `(bytes_received, sender_addr)`.
    pub fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> RecvFromFuture<'a> {
        RecvFromFuture { socket: self, buf }
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let fd = self.source.raw();
        // IoSource Drop deregisters from the reactor first; then we close fd.
        // SAFETY: we own `fd` exclusively; Drop runs at most once.
        unsafe { libc::close(fd) };
    }
}

// ── SendToFuture ──────────────────────────────────────────────────────────────

/// Future returned by [`UdpSocket::send_to`].
pub struct SendToFuture<'a> {
    socket: &'a UdpSocket,
    buf: &'a [u8],
    target: SocketAddr,
}

impl<'a> Future for SendToFuture<'a> {
    type Output = io::Result<usize>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match try_send_to(self.socket.source.raw(), self.buf, self.target) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Buffer full — wait for WRITABLE, then retry.
                match Pin::new(&mut self.socket.source.writable()).poll(cx) {
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
                }
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

// ── RecvFromFuture ────────────────────────────────────────────────────────────

/// Future returned by [`UdpSocket::recv_from`].
pub struct RecvFromFuture<'a> {
    socket: &'a UdpSocket,
    buf: &'a mut [u8],
}

impl<'a> Future for RecvFromFuture<'a> {
    type Output = io::Result<(usize, SocketAddr)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fd = self.socket.source.raw();
        match try_recv_from(fd, self.buf) {
            Ok(result) => Poll::Ready(Ok(result)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No data yet — register waker and wait for READABLE.
                match Pin::new(&mut self.socket.source.readable()).poll(cx) {
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
                }
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

// ── Unix helpers ──────────────────────────────────────────────────────────────

/// Create a UDP socket appropriate for `addr`'s family.
fn create_udp_socket(addr: SocketAddr) -> io::Result<i32> {
    let family = match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    // SAFETY: documented syscall with valid AF_INET/AF_INET6 + SOCK_DGRAM.
    let fd = unsafe { libc::socket(family, libc::SOCK_DGRAM, 0) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Bind `fd` to `addr`.
fn bind_socket(fd: i32, addr: SocketAddr) -> io::Result<()> {
    let (sa_ptr, sa_len) = socketaddr_to_raw(addr);
    // SAFETY: `fd` is a valid unbound socket; `sa_ptr`/`sa_len` are correct.
    let rc = unsafe { libc::bind(fd, sa_ptr, sa_len) };
    // SAFETY: reclaims the Box created by `socketaddr_to_raw`.
    unsafe { reclaim_raw_sockaddr(sa_ptr, addr) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Non-blocking `sendto`. Returns `Ok(n)` or `Err(WouldBlock)`.
fn try_send_to(fd: i32, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
    let (sa_ptr, sa_len) = socketaddr_to_raw(target);
    // SAFETY: `fd` is a valid UDP socket; `buf` is a valid readable slice;
    // `sa_ptr`/`sa_len` describe a valid sockaddr for `target`.
    let n = unsafe {
        libc::sendto(
            fd,
            buf.as_ptr() as *const libc::c_void,
            buf.len(),
            0, // flags
            sa_ptr,
            sa_len,
        )
    };
    // SAFETY: reclaims the Box created by `socketaddr_to_raw`.
    unsafe { reclaim_raw_sockaddr(sa_ptr, target) };
    if n == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

/// Non-blocking `recvfrom`. Returns `Ok((n, sender))` or `Err(WouldBlock)`.
fn try_recv_from(fd: i32, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&addr) as libc::socklen_t;
    // SAFETY: `fd` is a valid UDP socket; `buf` is a valid writable slice;
    // `addr` is zeroed and large enough for both address families.
    let n = unsafe {
        libc::recvfrom(
            fd,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
            0, // flags
            &mut addr as *mut _ as *mut libc::sockaddr,
            &mut len,
        )
    };
    if n == -1 {
        return Err(io::Error::last_os_error());
    }
    let sender = sockaddr_to_socketaddr(&addr, len)?;
    Ok((n as usize, sender))
}

/// Query the local address of `fd` via `getsockname`.
fn raw_local_addr(fd: i32) -> io::Result<SocketAddr> {
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&addr) as libc::socklen_t;
    // SAFETY: `fd` is a valid bound socket; `addr` buffer is large enough.
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
    use crate::executor::block_on_with_spawn;

    #[test]
    fn bind_and_local_addr() {
        let sock = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).expect("bind failed");
        let addr = sock.local_addr().expect("local_addr failed");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(addr.port() > 0);
    }

    #[test]
    fn send_to_and_recv_from() {
        block_on_with_spawn(async {
            let receiver = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let recv_addr = receiver.local_addr().unwrap();

            let sender = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();

            // Send a datagram.
            let msg = b"ping";
            let n = sender.send_to(msg, recv_addr).await.unwrap();
            assert_eq!(n, msg.len());

            // Receive it.
            let mut buf = [0u8; 16];
            let (n, from) = receiver.recv_from(&mut buf).await.unwrap();
            assert_eq!(n, msg.len());
            assert_eq!(&buf[..n], msg);
            // `from` should be the sender's address.
            assert_eq!(from.ip(), sender.local_addr().unwrap().ip());
        });
    }

    #[test]
    fn udp_echo_round_trip() {
        block_on_with_spawn(async {
            let server = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let server_addr = server.local_addr().unwrap();
            let client = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();

            // Client sends, server echoes back.
            client.send_to(b"hello", server_addr).await.unwrap();

            let mut buf = [0u8; 16];
            let (n, from) = server.recv_from(&mut buf).await.unwrap();
            server.send_to(&buf[..n], from).await.unwrap();

            let mut reply = [0u8; 16];
            let (rn, _) = client.recv_from(&mut reply).await.unwrap();
            assert_eq!(&reply[..rn], b"hello");
        });
    }
}
