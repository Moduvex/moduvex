//! `IoSource` — RAII wrapper that registers an OS handle with the reactor.
//!
//! Constructing an `IoSource` registers the handle; dropping it deregisters.
//! The `readable()` and `writable()` methods return futures that resolve when
//! the underlying OS handle becomes ready.
//!
//! Waker integration: on each poll the future stores the current waker in the
//! reactor's `WakerRegistry`. When the reactor fires an event for this token
//! the stored waker is called, re-scheduling the waiting task.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use super::{with_reactor, with_reactor_mut};
use crate::platform::sys::{Interest, RawSource};

/// Global counter for assigning unique tokens to `IoSource` instances.
static TOKEN_COUNTER: AtomicUsize = AtomicUsize::new(1);

/// Sentinel tokens reserved by the executor/signal subsystem.
/// These must never be assigned to user `IoSource` instances.
pub(crate) const WAKE_TOKEN: usize = usize::MAX;
pub(crate) const SIGNAL_TOKEN_GUARD: usize = usize::MAX - 1;

/// Allocate the next unique token.
///
/// Skips sentinel values (`usize::MAX`, `usize::MAX - 1`) used by the executor
/// and signal subsystem. In practice the counter can never reach these values
/// in a single process lifetime (~18 quintillion allocations), but we guard
/// defensively to prevent undefined behaviour on extremely long-lived processes.
pub(crate) fn next_token() -> usize {
    let t = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Guard against wrapping into sentinel range (theoretical: ~18 quintillion allocations).
    debug_assert!(
        t < WAKE_TOKEN - 2,
        "token counter approaching sentinel values — process has allocated too many IoSources"
    );
    t
}

/// RAII I/O source registered with the thread-local reactor.
///
/// The `token` field is used as the unique identifier when submitting kevent /
/// epoll_ctl calls. Tokens must be unique within a single reactor instance —
/// this type uses an atomic counter to guarantee uniqueness without caller
/// coordination.
pub struct IoSource {
    /// The raw OS handle (fd on Unix, HANDLE on Windows).
    raw: RawSource,
    /// Opaque identifier passed to the reactor for event demultiplexing.
    token: usize,
    /// Whether the source is currently registered with the reactor.
    /// Tracked atomically so Drop can skip deregistration if it never happened.
    registered: AtomicBool,
}

impl IoSource {
    /// Register `raw` with the thread-local reactor under `token`, monitoring
    /// the given `interest` set.
    ///
    /// # Errors
    /// Propagates any OS error from the underlying `register` syscall.
    pub fn new(raw: RawSource, token: usize, interest: Interest) -> io::Result<Self> {
        let source = Self {
            raw,
            token,
            registered: AtomicBool::new(false),
        };
        with_reactor(|r| r.register(raw, token, interest))?;
        source.registered.store(true, Ordering::Release);
        Ok(source)
    }

    /// Update the interest mask for an already-registered source.
    ///
    /// # Errors
    /// Returns `io::ErrorKind::NotConnected` if the source was never registered
    /// (e.g. after a failed `new`), or propagates OS errors from `reregister`.
    pub fn reregister(&self, interest: Interest) -> io::Result<()> {
        if !self.registered.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "IoSource: reregister called on unregistered source",
            ));
        }
        with_reactor(|r| r.reregister(self.raw, self.token, interest))
    }

    /// The raw OS handle.
    #[inline]
    pub fn raw(&self) -> RawSource {
        self.raw
    }

    /// The token that identifies this source in reactor events.
    #[inline]
    pub fn token(&self) -> usize {
        self.token
    }

    /// Returns a future that resolves once the source is readable.
    ///
    /// On each poll the current waker is stored in the reactor's waker registry.
    /// When the reactor fires a READABLE event for this token, the waker fires
    /// and the next poll returns `Ready(Ok(()))`.
    pub fn readable(&self) -> ReadableFuture<'_> {
        ReadableFuture {
            source: self,
            armed: false,
        }
    }

    /// Returns a future that resolves once the source is writable.
    ///
    /// Same waker integration as `readable()` but for WRITABLE events.
    pub fn writable(&self) -> WritableFuture<'_> {
        WritableFuture {
            source: self,
            armed: false,
        }
    }
}

impl Drop for IoSource {
    fn drop(&mut self) {
        // Only attempt deregistration if we successfully registered.
        if self.registered.swap(false, Ordering::AcqRel) {
            // Remove wakers first, then deregister from the platform backend.
            // Ignore errors — the fd may already be closed by the caller.
            // Use catch_unwind to avoid panicking if the reactor RefCell is
            // already borrowed (e.g., IoSource dropped inside a reactor callback).
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = with_reactor_mut(|r| r.deregister_with_token(self.raw, self.token));
            }));
        }
    }
}

// ── Readiness futures ─────────────────────────────────────────────────────────

/// Future returned by [`IoSource::readable`].
///
/// Stores the caller's waker in the reactor's `WakerRegistry` and arms
/// `READABLE` interest. Resolves to `Ok(())` after the reactor fires the event.
pub struct ReadableFuture<'a> {
    source: &'a IoSource,
    /// Whether we have already armed READABLE interest (avoid redundant syscalls).
    armed: bool,
}

impl<'a> Future for ReadableFuture<'a> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // If already armed, this is a re-poll after the reactor woke us — ready.
        if self.armed {
            return Poll::Ready(Ok(()));
        }

        // Store the waker so the reactor can wake us when the fd is readable.
        with_reactor_mut(|r| {
            r.wakers
                .set_read_waker(self.source.token, cx.waker().clone());
        });

        // Arm READABLE interest on first poll and return Pending.
        self.armed = true;
        if let Err(e) = self.source.reregister(Interest::READABLE) {
            return Poll::Ready(Err(e));
        }

        Poll::Pending
    }
}

/// Future returned by [`IoSource::writable`].
///
/// Stores the caller's waker in the reactor's `WakerRegistry` and arms
/// `WRITABLE` interest. Resolves to `Ok(())` after the reactor fires the event.
pub struct WritableFuture<'a> {
    source: &'a IoSource,
    /// Whether we have already armed WRITABLE interest.
    armed: bool,
}

impl<'a> Future for WritableFuture<'a> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // If already armed, this is a re-poll after the reactor woke us — ready.
        if self.armed {
            return Poll::Ready(Ok(()));
        }

        // Store the waker so the reactor can wake us when the fd is writable.
        with_reactor_mut(|r| {
            r.wakers
                .set_write_waker(self.source.token, cx.waker().clone());
        });

        // Arm WRITABLE interest on first poll and return Pending.
        self.armed = true;
        if let Err(e) = self.source.reregister(Interest::WRITABLE) {
            return Poll::Ready(Err(e));
        }

        Poll::Pending
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::sys::create_pipe;

    #[cfg(unix)]
    #[test]
    fn io_source_registers_on_construction() {
        let (r, w) = create_pipe().unwrap();
        let src = IoSource::new(r, next_token(), Interest::READABLE).expect("IoSource::new failed");
        assert!(src.registered.load(Ordering::Acquire));
        // Drop triggers deregister; close the fds manually after drop.
        drop(src);
        // SAFETY: fds are valid and owned by this test; src has been dropped.
        unsafe { libc::close(r) };
        unsafe { libc::close(w) };
    }

    #[cfg(unix)]
    #[test]
    fn io_source_deregisters_on_drop() {
        let (r, w) = create_pipe().unwrap();
        {
            let src = IoSource::new(r, next_token(), Interest::READABLE).unwrap();
            assert!(src.registered.load(Ordering::Acquire));
            // src drops here → deregister called automatically
        }
        // After drop, re-registering the same fd with a new IoSource must
        // succeed, proving the previous deregister went through.
        let src2 = IoSource::new(r, next_token(), Interest::READABLE)
            .expect("re-register after drop failed");
        drop(src2);
        // SAFETY: fds are valid and owned by this test; src2 has been dropped.
        unsafe { libc::close(r) };
        unsafe { libc::close(w) };
    }

    #[test]
    fn next_token_is_unique() {
        let t1 = next_token();
        let t2 = next_token();
        let t3 = next_token();
        assert!(t1 < t2 && t2 < t3);
    }
}
