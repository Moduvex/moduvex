//! Async networking: TCP listener, TCP stream, UDP socket.
//!
//! # Traits
//! - [`AsyncRead`] — poll-based non-blocking read
//! - [`AsyncWrite`] — poll-based non-blocking write + flush + shutdown
//!
//! # Types
//! - [`TcpListener`] — accepts incoming TCP connections
//! - [`TcpStream`]   — bidirectional async byte stream
//! - [`UdpSocket`]   — connectionless async datagram socket

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

pub mod tcp_listener;
pub mod tcp_stream;
pub mod udp_socket;

pub use tcp_listener::TcpListener;
pub use tcp_stream::TcpStream;
pub use udp_socket::UdpSocket;

// ── AsyncRead ─────────────────────────────────────────────────────────────────

/// Async version of `std::io::Read`.
///
/// The poll method must be called with a pinned `Self` and a `Context`.
/// It returns `Poll::Ready(Ok(n))` when `n` bytes have been written into
/// `buf`, or `Poll::Pending` when no bytes are available yet (the waker
/// will be called when data arrives).
pub trait AsyncRead {
    /// Attempt to read bytes into `buf`.
    ///
    /// On success returns `Poll::Ready(Ok(n))` where `n` is the number of
    /// bytes read. `n == 0` signals EOF. Returns `Poll::Pending` and
    /// arranges for the waker to be called on the next read-readiness event.
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>>;
}

// ── AsyncWrite ────────────────────────────────────────────────────────────────

/// Async version of `std::io::Write`.
///
/// All three poll methods follow the same contract: `Poll::Ready(Ok(...))`
/// on success, `Poll::Pending` when the underlying resource is not ready
/// (with the waker arranged to fire when it becomes ready).
pub trait AsyncWrite {
    /// Attempt to write bytes from `buf` into the sink.
    ///
    /// Returns the number of bytes written. May be less than `buf.len()`.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>>;

    /// Flush any internal buffers to the OS.
    ///
    /// For kernel-backed sockets this is a no-op that returns `Ready(Ok(()))`.
    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>>;

    /// Initiate a half-close: shut down the write side of the connection.
    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>>;
}
