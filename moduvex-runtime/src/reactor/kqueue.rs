//! macOS / FreeBSD kqueue-based reactor backend.
//!
//! Uses `EV_CLEAR` (edge-triggered) mode. Read and write readiness are tracked
//! via separate `EVFILT_READ` / `EVFILT_WRITE` filters because kqueue treats
//! them as independent event streams — unlike epoll which packs them into a
//! single bitmask per fd.
//!
//! This file is only compiled on macOS/FreeBSD — the outer `mod kqueue`
//! declaration in `reactor/mod.rs` is guarded by the appropriate `#[cfg]`.

use std::io;
use std::os::unix::io::RawFd;

use libc::{
    kevent, kqueue,
    timespec,
    EV_ADD, EV_CLEAR, EV_DELETE, EV_ERROR,
    EVFILT_READ, EVFILT_WRITE,
};

use crate::platform::sys::{Event, Events, Interest, RawSource};
use super::ReactorBackend;

/// Maximum events collected per `poll` call.
const MAX_EVENTS: usize = 1024;

/// Reactor backend backed by BSD/macOS `kqueue`.
pub(crate) struct KqueueReactor {
    /// The kqueue file descriptor created by `kqueue()`.
    kq_fd: RawFd,
}

impl KqueueReactor {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: `kqueue()` takes no arguments and is always safe to call.
        // It returns a new kqueue fd on success or -1 on failure.
        let fd = unsafe { kqueue() };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { kq_fd: fd })
    }

    /// Submit one or more `kevent` change entries to the kernel.
    ///
    /// `changes` — slice of change-list entries to submit.
    /// Returns `Err` if the kernel rejects any entry (checked via `EV_ERROR`).
    fn kevent_submit(&self, changes: &[libc::kevent]) -> io::Result<()> {
        // SAFETY: `self.kq_fd` is a valid kqueue fd. `changes` is a correctly
        // formed slice; we cast it to a const pointer and pass its length.
        // We pass null/0 for the event-list because we only want to submit
        // changes, not collect ready events, in this helper.
        let rc = unsafe {
            kevent(
                self.kq_fd,
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(), // no output events
                0,
                std::ptr::null(),     // no timeout → non-blocking change submission
            )
        };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        // kqueue reports per-entry errors via EV_ERROR in the output list.
        // Since we requested 0 output events the kernel signals batch errors
        // through the return value alone, which we already checked.
        Ok(())
    }

    /// Build a single `kevent` struct for `EV_ADD | EV_CLEAR` (register).
    #[inline]
    fn make_add_event(source: RawSource, filter: i16, token: usize) -> libc::kevent {
        libc::kevent {
            ident:  source as libc::uintptr_t,
            filter,
            flags:  (EV_ADD | EV_CLEAR) as u16,
            fflags: 0,
            data:   0,
            udata:  token as *mut libc::c_void,
        }
    }

    /// Build a single `kevent` struct for `EV_DELETE` (deregister).
    #[inline]
    fn make_del_event(source: RawSource, filter: i16) -> libc::kevent {
        libc::kevent {
            ident:  source as libc::uintptr_t,
            filter,
            flags:  EV_DELETE as u16,
            fflags: 0,
            data:   0,
            udata:  std::ptr::null_mut(),
        }
    }
}

impl ReactorBackend for KqueueReactor {
    fn new() -> io::Result<Self> {
        KqueueReactor::new()
    }

    fn register(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()> {
        let mut changes: Vec<libc::kevent> = Vec::with_capacity(2);
        if interest.is_readable() {
            changes.push(Self::make_add_event(source, EVFILT_READ, token));
        }
        if interest.is_writable() {
            changes.push(Self::make_add_event(source, EVFILT_WRITE, token));
        }
        self.kevent_submit(&changes)
    }

    fn reregister(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()> {
        // kqueue's `EV_ADD` on an already-registered filter acts as a modify,
        // so re-register is identical to register.
        self.register(source, token, interest)
    }

    fn deregister(&self, source: RawSource) -> io::Result<()> {
        // Attempt to delete both filters; the fd may only have one registered.
        // Silently ignore ENOENT (filter was never added).
        let changes = [
            Self::make_del_event(source, EVFILT_READ),
            Self::make_del_event(source, EVFILT_WRITE),
        ];
        // SAFETY: Same invariants as `kevent_submit`. We pass a 2-element
        // output buffer so the kernel can report per-entry EV_ERROR results
        // for missing filters without failing the whole call.
        let mut out: [libc::kevent; 2] = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            kevent(
                self.kq_fd,
                changes.as_ptr(),
                changes.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
                std::ptr::null(),
            )
        };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        // Check per-entry errors — ENOENT is acceptable (filter not registered).
        for entry in &out[..rc as usize] {
            if entry.flags & EV_ERROR as u16 != 0 && entry.data != libc::ENOENT as isize {
                return Err(io::Error::from_raw_os_error(entry.data as i32));
            }
        }
        Ok(())
    }

    fn poll(&self, events: &mut Events, timeout_ms: Option<u64>) -> io::Result<usize> {
        let cap = MAX_EVENTS.max(events.capacity());
        let mut raw: Vec<libc::kevent> = Vec::with_capacity(cap);

        let ts_storage;
        let ts_ptr = match timeout_ms {
            Some(ms) => {
                ts_storage = timespec {
                    tv_sec:  (ms / 1000) as libc::time_t,
                    tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long,
                };
                &ts_storage as *const timespec
            }
            None => std::ptr::null(), // block indefinitely
        };

        // SAFETY: `raw` has `cap` slots of uninitialised memory. `kevent`
        // writes exactly `n` initialised entries (0 ≤ n ≤ cap) and returns `n`.
        // We set `raw.len()` to `n` before accessing any element.
        let n = unsafe {
            kevent(
                self.kq_fd,
                std::ptr::null(),     // no changes to submit
                0,
                raw.as_mut_ptr(),
                cap as i32,
                ts_ptr,
            )
        };
        if n == -1 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(0);
            }
            return Err(err);
        }
        let n = n as usize;
        // SAFETY: `kevent` guarantees the first `n` entries are initialised.
        unsafe { raw.set_len(n) };

        events.clear();
        events.reserve(n);
        for kev in &raw {
            // Skip entries that carry error flags.
            if kev.flags & EV_ERROR as u16 != 0 {
                continue;
            }
            let token = kev.udata as usize;
            let readable = kev.filter == EVFILT_READ;
            let writable = kev.filter == EVFILT_WRITE;
            events.push(Event::new(token, readable, writable));
        }
        Ok(events.len())
    }
}

impl Drop for KqueueReactor {
    fn drop(&mut self) {
        // SAFETY: `self.kq_fd` is a valid fd we own exclusively. After `close`
        // the fd is invalid and `self` is being dropped so it will not be used.
        unsafe { libc::close(self.kq_fd) };
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::sys::{create_pipe, events_with_capacity};

    #[test]
    fn kqueue_reactor_creates_successfully() {
        KqueueReactor::new().expect("kqueue() should succeed");
    }

    #[test]
    fn register_and_deregister_pipe_read_end() {
        let reactor = KqueueReactor::new().unwrap();
        let (r, w) = create_pipe().unwrap();
        reactor.register(r, 1, Interest::READABLE).expect("register failed");
        reactor.deregister(r).expect("deregister failed");
        unsafe { libc::close(r) };
        unsafe { libc::close(w) };
    }

    #[test]
    fn poll_detects_readable_pipe() {
        let reactor = KqueueReactor::new().unwrap();
        let (r, w) = create_pipe().unwrap();
        reactor.register(r, 77, Interest::READABLE).unwrap();

        // Make the read-end readable.
        let byte: u8 = 0xFF;
        // SAFETY: `w` is a valid write fd; pointer is valid for 1 byte.
        unsafe { libc::write(w, &byte as *const u8 as *const _, 1) };

        let mut events = events_with_capacity(16);
        let n = reactor.poll(&mut events, Some(200)).expect("poll failed");
        assert!(n >= 1);
        let ev = events.iter().find(|e| e.token == 77).expect("token 77 not found");
        assert!(ev.readable);

        unsafe { libc::close(r) };
        unsafe { libc::close(w) };
    }
}
