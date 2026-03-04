//! Linux epoll-based reactor backend.
//!
//! Uses edge-triggered + one-shot mode (`EPOLLET | EPOLLONESHOT`) so each
//! event fires exactly once. Callers must re-arm a source via `reregister`
//! after processing an event to receive the next notification.
//!
//! This file is only compiled on Linux — the outer `mod epoll` declaration
//! in `reactor/mod.rs` is guarded by `#[cfg(target_os = "linux")]`.

use std::io;
use std::os::unix::io::RawFd;

use libc::{
    epoll_create1, epoll_ctl, epoll_event, epoll_wait, EPOLLET, EPOLLIN, EPOLLONESHOT, EPOLLOUT,
    EPOLL_CLOEXEC, EPOLL_CTL_ADD, EPOLL_CTL_DEL, EPOLL_CTL_MOD,
};

use super::ReactorBackend;
use crate::platform::sys::{Event, Events, Interest, RawSource};

/// Default maximum events to collect per `poll` call.
const MAX_EVENTS: usize = 1024;

/// Reactor backend backed by Linux `epoll`.
pub(crate) struct EpollReactor {
    /// The epoll file descriptor created by `epoll_create1`.
    epoll_fd: RawFd,
}

impl EpollReactor {
    /// Create a new `EpollReactor`.
    ///
    /// Opens a new epoll instance with `EPOLL_CLOEXEC` so child processes do
    /// not accidentally inherit it.
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: `epoll_create1` is always safe to call; the only argument is
        // a flags value. `EPOLL_CLOEXEC` is a well-known, documented flag.
        // On success it returns a non-negative fd; on failure it returns -1.
        let fd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { epoll_fd: fd })
    }

    /// Build an `epoll_event` value from a token and interest mask.
    ///
    /// We always use `EPOLLET | EPOLLONESHOT` for precise, one-shot
    /// edge-triggered notifications.
    fn build_event(token: usize, interest: Interest) -> epoll_event {
        let mut events: u32 = EPOLLET as u32 | EPOLLONESHOT as u32;
        if interest.is_readable() {
            events |= EPOLLIN as u32;
        }
        if interest.is_writable() {
            events |= EPOLLOUT as u32;
        }
        epoll_event {
            events,
            // The `u64` data field carries the token so we can identify the
            // source when the event fires. `epoll_event` is `#[repr(packed)]`
            // on Linux so we use the `u64` union variant.
            u64: token as u64,
        }
    }
}

impl ReactorBackend for EpollReactor {
    fn new() -> io::Result<Self> {
        EpollReactor::new()
    }

    fn register(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()> {
        let mut ev = Self::build_event(token, interest);
        // SAFETY: `self.epoll_fd` is the valid epoll fd created in `new()`.
        // `source` is a valid open fd supplied by the caller.
        // `&mut ev` points to a correctly initialised `epoll_event`.
        let rc = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_ADD, source, &mut ev) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn reregister(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()> {
        let mut ev = Self::build_event(token, interest);
        // SAFETY: Same invariants as `register`. `EPOLL_CTL_MOD` is only valid
        // if `source` was previously registered — callers must uphold this.
        let rc = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_MOD, source, &mut ev) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn deregister(&self, source: RawSource) -> io::Result<()> {
        // SAFETY: `EPOLL_CTL_DEL` ignores the event pointer on Linux ≥ 2.6.9;
        // passing null is documented as safe. `source` must have been
        // registered previously — callers must uphold this invariant.
        let rc = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_DEL, source, std::ptr::null_mut()) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn poll(&self, events: &mut Events, timeout_ms: Option<u64>) -> io::Result<usize> {
        // Reuse or grow the scratch buffer; never shrink below MAX_EVENTS.
        let cap = MAX_EVENTS.max(events.capacity());
        let mut raw: Vec<epoll_event> = Vec::with_capacity(cap);

        let timeout = match timeout_ms {
            Some(ms) => ms.min(i32::MAX as u64) as i32,
            None => -1, // block indefinitely
        };

        // SAFETY: `raw` has at least `cap` slots of uninitialised memory.
        // `epoll_wait` fills exactly `n` of them and returns `n` (0 ≤ n ≤ cap).
        // We immediately set the length to `n` before reading any element.
        let n = unsafe { epoll_wait(self.epoll_fd, raw.as_mut_ptr(), cap as i32, timeout) };
        if n == -1 {
            let err = io::Error::last_os_error();
            // EINTR is not a real error — the caller should retry.
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(0);
            }
            return Err(err);
        }
        let n = n as usize;
        // SAFETY: `epoll_wait` guarantees the first `n` elements are initialised.
        unsafe { raw.set_len(n) };

        events.clear();
        events.reserve(n);
        for ev in &raw {
            let token = ev.u64 as usize;
            let readable = ev.events & EPOLLIN as u32 != 0;
            let writable = ev.events & EPOLLOUT as u32 != 0;
            events.push(Event::new(token, readable, writable));
        }
        Ok(n)
    }
}

impl Drop for EpollReactor {
    fn drop(&mut self) {
        // SAFETY: `self.epoll_fd` is a valid fd that we own exclusively.
        // After `close` the fd value is invalid and will not be used again
        // because `self` is being dropped.
        unsafe { libc::close(self.epoll_fd) };
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::sys::{create_pipe, events_with_capacity};

    #[test]
    fn epoll_reactor_creates_successfully() {
        EpollReactor::new().expect("epoll_create1 should succeed");
    }

    #[test]
    fn register_and_deregister_pipe_read_end() {
        let reactor = EpollReactor::new().unwrap();
        let (r, w) = create_pipe().unwrap();
        reactor
            .register(r, 1, Interest::READABLE)
            .expect("register failed");
        reactor.deregister(r).expect("deregister failed");
        unsafe { libc::close(r) };
        unsafe { libc::close(w) };
    }

    #[test]
    fn poll_detects_readable_pipe() {
        let reactor = EpollReactor::new().unwrap();
        let (r, w) = create_pipe().unwrap();
        reactor.register(r, 99, Interest::READABLE).unwrap();

        // Make the read-end readable by writing a byte to the write-end.
        let byte: u8 = 1;
        // SAFETY: `w` is a valid write fd; the pointer is valid for 1 byte.
        unsafe { libc::write(w, &byte as *const u8 as *const _, 1) };

        let mut events = events_with_capacity(16);
        let n = reactor.poll(&mut events, Some(100)).expect("poll failed");
        assert_eq!(n, 1);
        assert_eq!(events[0].token, 99);
        assert!(events[0].readable);

        unsafe { libc::close(r) };
        unsafe { libc::close(w) };
    }
}
