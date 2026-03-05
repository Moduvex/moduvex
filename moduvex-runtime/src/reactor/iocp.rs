//! Windows reactor backend using WSAPoll for socket readiness notification.
//!
//! WSAPoll is the Windows equivalent of `poll(2)` on Unix. It provides a
//! simple, synchronous polling API that maps cleanly to the `ReactorBackend`
//! trait. This is sufficient for moderate concurrency (~1K sockets).
//!
//! # Design choice: WSAPoll vs AFD-IOCP
//!
//! | Feature              | WSAPoll (this impl)   | AFD-IOCP (future v2)      |
//! |----------------------|-----------------------|---------------------------|
//! | Complexity           | Low                   | High (driver-level)       |
//! | Max sockets          | ~1,000                | Hundreds of thousands     |
//! | Winsock init         | Required              | Required                  |
//! | mio-style IOCP       | No                    | Yes                       |
//!
//! **Upgrade path for v2.0:** Replace `IocpReactor` with an AFD-based IOCP
//! backend using `CreateIoCompletionPort` + `GetQueuedCompletionStatusEx` +
//! per-socket `OVERLAPPED` structures (see mio's `sys/windows/` for patterns).
//!
//! This file is only compiled on Windows — the outer `mod iocp` declaration
//! in `reactor/mod.rs` is guarded by `#[cfg(target_os = "windows")]`.

use std::io;
use std::sync::Once;

use super::ReactorBackend;
use crate::platform::sys::{Event, Events, Interest, RawSource};

// ── WSAPoll event flags (Winsock2) ────────────────────────────────────────────
// These mirror the POLLXXX constants from <winsock2.h>.

/// Ready for reading.
const POLLIN: i16 = 0x0100;
/// Ready for writing.
const POLLOUT: i16 = 0x0010;
/// Hangup detected (readable).
const POLLHUP: i16 = 0x0002;
/// Error condition (readable + writable).
const POLLERR: i16 = 0x0001;

// ── IocpReactor ───────────────────────────────────────────────────────────────

/// Windows reactor using WSAPoll for socket readiness notification.
///
/// Tracks registered sockets in a parallel pair of `Vec`s (`fds` and `tokens`)
/// so that WSAPoll can be called directly on `fds` without extra indirection.
///
/// # Scalability
/// WSAPoll scales to approximately 1,000 concurrent sockets. For higher
/// concurrency, replace with an AFD-based IOCP reactor (see module-level docs).
pub(crate) struct IocpReactor {
    /// Registered socket descriptors for WSAPoll (index-matched with `tokens`).
    fds: Vec<windows_sys::Win32::Networking::WinSock::WSAPOLLFD>,
    /// Caller-supplied token for each entry (index-matched with `fds`).
    tokens: Vec<usize>,
}

impl ReactorBackend for IocpReactor {
    fn new() -> io::Result<Self> {
        // Winsock must be initialised before any WSA call.
        init_winsock()?;
        Ok(Self {
            fds: Vec::with_capacity(64),
            tokens: Vec::with_capacity(64),
        })
    }

    fn register(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()> {
        // SAFETY: Interior mutability via raw pointer — consistent with epoll/kqueue backends.
        // The trait takes `&self` so the reactor can live behind a `RefCell`; callers
        // guarantee no concurrent calls from multiple threads on the same reactor instance.
        let this = unsafe { &mut *(self as *const Self as *mut Self) };
        this.fds.push(windows_sys::Win32::Networking::WinSock::WSAPOLLFD {
            fd: source as usize,
            events: interest_to_wsa(interest),
            revents: 0,
        });
        this.tokens.push(token);
        Ok(())
    }

    fn reregister(&self, source: RawSource, token: usize, interest: Interest) -> io::Result<()> {
        // SAFETY: Same as `register` — single-threaded reactor access guaranteed by callers.
        let this = unsafe { &mut *(self as *const Self as *mut Self) };
        for (i, fd) in this.fds.iter_mut().enumerate() {
            if fd.fd == source as usize && this.tokens[i] == token {
                fd.events = interest_to_wsa(interest);
                return Ok(());
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "source not registered"))
    }

    fn deregister(&self, source: RawSource) -> io::Result<()> {
        // SAFETY: Same as `register`.
        let this = unsafe { &mut *(self as *const Self as *mut Self) };
        if let Some(pos) = this.fds.iter().position(|fd| fd.fd == source as usize) {
            this.fds.swap_remove(pos);
            this.tokens.swap_remove(pos);
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "source not registered"))
        }
    }

    fn poll(&self, events: &mut Events, timeout_ms: Option<u64>) -> io::Result<usize> {
        // SAFETY: Same as `register`.
        let this = unsafe { &mut *(self as *const Self as *mut Self) };
        events.clear();

        // No sockets registered — sleep for the timeout period and return.
        if this.fds.is_empty() {
            if let Some(ms) = timeout_ms {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            return Ok(0);
        }

        // WSAPoll uses i32 timeout: -1 = block indefinitely, 0 = immediate return.
        let timeout = timeout_ms.map(|ms| ms as i32).unwrap_or(-1);

        // SAFETY: `this.fds` is a valid slice of WSAPOLLFD structures. WSAPoll
        // reads `nfds` entries from the array and writes `revents` in-place.
        let n = unsafe {
            windows_sys::Win32::Networking::WinSock::WSAPoll(
                this.fds.as_mut_ptr(),
                this.fds.len() as u32,
                timeout,
            )
        };

        if n < 0 {
            return Err(io::Error::last_os_error());
        }

        // Translate WSAPoll revents into our Event type.
        for i in 0..this.fds.len() {
            let revents = this.fds[i].revents;
            if revents != 0 {
                let readable = revents & (POLLIN | POLLHUP | POLLERR) != 0;
                let writable = revents & (POLLOUT | POLLHUP | POLLERR) != 0;
                events.push(Event::new(this.tokens[i], readable, writable));
                // Clear revents for next poll cycle.
                this.fds[i].revents = 0;
            }
        }

        Ok(events.len())
    }
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Convert our `Interest` bitmask to WSAPoll event flags.
#[inline]
fn interest_to_wsa(interest: Interest) -> i16 {
    let mut events = 0i16;
    if interest.is_readable() {
        events |= POLLIN;
    }
    if interest.is_writable() {
        events |= POLLOUT;
    }
    events
}

/// Initialise Winsock 2.2 exactly once per process.
///
/// Must be called before any WSA function. Uses `std::sync::Once` to ensure
/// idempotency across multiple `IocpReactor::new()` calls.
fn init_winsock() -> io::Result<()> {
    static INIT: Once = Once::new();
    static mut INIT_RESULT: io::Result<()> = Ok(());

    INIT.call_once(|| {
        // SAFETY: `WSAStartup` is safe to call; `wsa_data` is stack-allocated
        // and sized correctly for `WSADATA`. On success Winsock is ready.
        let mut wsa_data = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            windows_sys::Win32::Networking::WinSock::WSAStartup(
                0x0202, // request Winsock 2.2
                &mut wsa_data,
            )
        };
        if ret != 0 {
            // SAFETY: written once inside `call_once`, read only after it completes.
            unsafe {
                INIT_RESULT = Err(io::Error::from_raw_os_error(ret));
            }
        }
    });

    // SAFETY: `INIT_RESULT` is written once (inside `call_once`) before this
    // read executes. After `call_once` returns, subsequent reads are safe.
    let result = unsafe { &INIT_RESULT };
    match result {
        Ok(()) => Ok(()),
        Err(e) => Err(io::Error::from_raw_os_error(
            e.raw_os_error().unwrap_or(0),
        )),
    }
}
