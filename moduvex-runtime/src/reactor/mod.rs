//! Reactor — I/O readiness event loop.
//!
//! The reactor multiplexes OS I/O events (epoll / kqueue / IOCP) onto async
//! tasks. Each thread owns exactly one reactor, accessed via the thread-local
//! `REACTOR`. The `with_reactor` helper provides safe, borrow-scoped access.
//!
//! Platform dispatch is done entirely at compile time via `cfg` attributes —
//! zero runtime overhead, no vtable.

use std::io;
use std::cell::RefCell;

use crate::platform::sys::{Events, Interest, RawSource};

// ── Platform-specific backends ────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod epoll;
#[cfg(target_os = "linux")]
use epoll::EpollReactor as PlatformReactor;

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
mod kqueue;
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
use kqueue::KqueueReactor as PlatformReactor;

#[cfg(target_os = "windows")]
mod iocp;
#[cfg(target_os = "windows")]
use iocp::IocpReactor as PlatformReactor;

pub mod source;
pub mod waker_registry;

pub use source::IoSource;
pub(crate) use waker_registry::WakerRegistry;

// ── ReactorBackend trait ──────────────────────────────────────────────────────

/// Abstraction over platform-specific I/O polling mechanisms.
///
/// Implementors: `EpollReactor` (Linux), `KqueueReactor` (macOS/BSD),
/// `IocpReactor` (Windows stub).
///
/// All methods take `&self` (shared ref) so the reactor can be held behind a
/// `RefCell` and lent immutably to concurrent borrows on the same thread.
pub(crate) trait ReactorBackend: Sized {
    /// Construct a new backend instance, opening the underlying OS resource.
    fn new() -> io::Result<Self>;

    /// Register `source` with the reactor under `token`, monitoring `interest`.
    ///
    /// `token` is an opaque caller-chosen `usize` returned verbatim in events.
    /// Callers must ensure `token` is unique within a single reactor instance.
    fn register(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()>;

    /// Update the interest mask of an already-registered source.
    fn reregister(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()>;

    /// Remove `source` from the reactor. Must be called before closing the fd.
    fn deregister(&self, source: RawSource) -> io::Result<()>;

    /// Block until at least one event is ready or `timeout_ms` elapses.
    ///
    /// Ready events are appended to `events` (cleared first). Returns the
    /// number of events collected.
    ///
    /// `timeout_ms = None` blocks indefinitely.
    /// `timeout_ms = Some(0)` returns immediately (non-blocking check).
    fn poll(&self, events: &mut Events, timeout_ms: Option<u64>) -> io::Result<usize>;
}

// ── Reactor wrapper ───────────────────────────────────────────────────────────

/// Thread-owned reactor that wraps the platform-specific backend.
///
/// Stored in a `RefCell` inside the thread-local so that mutable access can be
/// checked at runtime (panics on re-entrant borrow, which must not happen in
/// correct code).
pub struct Reactor {
    inner: PlatformReactor,
    /// Maps reactor tokens to task wakers. Fired when I/O becomes ready.
    pub(crate) wakers: WakerRegistry,
}

impl Reactor {
    /// Create a new `Reactor`, opening the underlying OS polling resource.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inner: PlatformReactor::new()?,
            wakers: WakerRegistry::new(),
        })
    }

    /// Register a raw source. Delegates to the platform backend.
    #[inline]
    pub fn register(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()> {
        self.inner.register(source, token, interest)
    }

    /// Re-register (update interest) a raw source.
    #[inline]
    pub fn reregister(
        &self,
        source: RawSource,
        token: usize,
        interest: Interest,
    ) -> io::Result<()> {
        self.inner.reregister(source, token, interest)
    }

    /// Deregister a raw source and remove its wakers from the registry.
    #[inline]
    pub fn deregister(&self, source: RawSource) -> io::Result<()> {
        self.inner.deregister(source)
    }

    /// Deregister a raw source and also clear its waker slots by token.
    ///
    /// Use this variant when you know the token (IoSource drop path).
    pub(crate) fn deregister_with_token(
        &mut self,
        source: RawSource,
        token: usize,
    ) -> io::Result<()> {
        self.wakers.remove_token(token);
        self.inner.deregister(source)
    }

    /// Poll for ready events and wake registered task wakers.
    ///
    /// Fills `events` from the platform backend, then fires any wakers whose
    /// tokens appear in the event list. Returns the number of events collected.
    pub fn poll(&mut self, events: &mut Events, timeout_ms: Option<u64>) -> io::Result<usize> {
        let n = self.inner.poll(events, timeout_ms)?;
        // Wake tasks registered for these events.
        for ev in events.iter() {
            self.wakers.wake_token(ev.token, ev.readable, ev.writable);
        }
        Ok(n)
    }

    /// Poll without waking (for executor's self-pipe drain path).
    ///
    /// The executor uses this when it wants raw events and handles waking itself.
    pub(crate) fn poll_raw(
        &self,
        events: &mut Events,
        timeout_ms: Option<u64>,
    ) -> io::Result<usize> {
        self.inner.poll(events, timeout_ms)
    }
}

// ── Thread-local reactor ──────────────────────────────────────────────────────

thread_local! {
    /// Per-thread reactor instance.
    ///
    /// Lazily initialised on first access. Panics if the platform backend
    /// fails to initialise (e.g. `kqueue()` / `epoll_create1()` returns an
    /// error), which would indicate a severe OS-level resource exhaustion.
    static REACTOR: RefCell<Reactor> = RefCell::new(
        Reactor::new().expect("failed to initialise platform reactor")
    );
}

/// Borrow the thread-local reactor for the duration of `f`.
///
/// # Panics
/// Panics if called re-entrantly on the same thread (i.e. from within another
/// `with_reactor` call on the same thread). This mirrors the contract of
/// `RefCell::borrow` and should never happen in correct executor code.
pub fn with_reactor<F, R>(f: F) -> R
where
    F: FnOnce(&Reactor) -> R,
{
    REACTOR.with(|cell| f(&cell.borrow()))
}

/// Mutably borrow the thread-local reactor for the duration of `f`.
///
/// Only the executor's poll loop should use this; all other callers use the
/// shared `with_reactor`.
pub(crate) fn with_reactor_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut Reactor) -> R,
{
    REACTOR.with(|cell| f(&mut cell.borrow_mut()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::sys::{create_pipe, events_with_capacity};

    #[test]
    fn reactor_initialises_via_thread_local() {
        // `with_reactor` must not panic — reactor initialised lazily here.
        with_reactor(|_r| {});
    }

    #[cfg(unix)]
    #[test]
    fn reactor_register_deregister_roundtrip() {
        let (r, w) = create_pipe().unwrap();
        with_reactor(|reactor| {
            reactor.register(r, 10, Interest::READABLE).expect("register");
            reactor.deregister(r).expect("deregister");
        });
        // SAFETY: fds are valid and owned by this test; deregistered above.
        unsafe { libc::close(r) };
        unsafe { libc::close(w) };
    }

    #[test]
    fn reactor_poll_timeout_zero_returns_immediately() {
        let mut events = events_with_capacity(16);
        with_reactor_mut(|reactor| {
            // No sources registered — timeout=0 must return immediately with 0.
            let n = reactor.poll(&mut events, Some(0)).expect("poll");
            assert_eq!(n, 0);
        });
    }
}
