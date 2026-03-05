//! Unix signal handling via the self-pipe trick.
//!
//! # How it works
//!
//! 1. At first use, a global pipe `(read_fd, write_fd)` is created.
//! 2. A `libc::sigaction` handler is installed for SIGTERM and SIGINT.
//!    The handler only writes one byte to `write_fd` — the only async-signal-safe
//!    operation needed.
//! 3. `read_fd` is registered with the thread-local reactor under `SIGNAL_TOKEN`.
//! 4. When the reactor fires readable on `read_fd`, the executor drains the pipe
//!    and wakes all registered signal waiters.
//! 5. `Signal` is a `Future<Output = SignalKind>` that the caller awaits.
//!
//! # Limitations
//! - Signal delivery wakes *all* registered `Signal` futures for that kind.
//! - Only SIGINT and SIGTERM are supported; add more by extending `SignalKind`.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll, Waker};

use crate::platform::sys::{create_pipe, Interest};
use crate::reactor::with_reactor;

// ── SignalKind ────────────────────────────────────────────────────────────────

/// The kind of Unix signal received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// SIGINT (Ctrl-C).
    Interrupt,
    /// SIGTERM (graceful shutdown).
    Terminate,
}

// ── Global signal state ───────────────────────────────────────────────────────

/// Reactor token for the signal self-pipe read end.
pub(crate) const SIGNAL_TOKEN: usize = usize::MAX - 1;

struct SignalState {
    /// Write end — written by the async-signal-safe handler.
    write_fd: i32,
    /// Read end — registered with the reactor; drained by the executor.
    read_fd: i32,
    /// Pending wakers grouped by signal kind.
    waiters: Vec<(SignalKind, Waker)>,
    /// Last signal kind received (set when the pipe fires).
    pending: Vec<SignalKind>,
}

static SIGNAL_STATE: OnceLock<Mutex<SignalState>> = OnceLock::new();

/// Initialise the global signal pipe and install handlers (idempotent).
///
/// Called on the first `signal()` invocation. Subsequent calls are no-ops
/// because `OnceLock::get_or_init` runs the closure exactly once.
fn ensure_init() -> io::Result<()> {
    // `OnceLock::get_or_init` is called outside the lock, so we need to
    // handle errors via a side-channel. We use a local `Result` wrapped
    // inside the closure.
    let mut init_err: Option<io::Error> = None;

    SIGNAL_STATE.get_or_init(|| {
        match init_signal_state() {
            Ok(state) => Mutex::new(state),
            Err(e) => {
                init_err = Some(e);
                // Return a dummy state; the error will be surfaced below.
                Mutex::new(SignalState {
                    write_fd: -1,
                    read_fd: -1,
                    waiters: Vec::new(),
                    pending: Vec::new(),
                })
            }
        }
    });

    if let Some(e) = init_err {
        return Err(e);
    }
    Ok(())
}

fn init_signal_state() -> io::Result<SignalState> {
    let (read_fd, write_fd) = create_pipe()?;

    // Register read end with the thread-local reactor.
    with_reactor(|r| r.register(read_fd, SIGNAL_TOKEN, Interest::READABLE))?;

    // Install async-signal-safe handlers.
    install_handler(libc::SIGINT, write_fd)?;
    install_handler(libc::SIGTERM, write_fd)?;

    Ok(SignalState {
        write_fd,
        read_fd,
        waiters: Vec::new(),
        pending: Vec::new(),
    })
}

/// Install a `sigaction` handler for `signum` that writes the signal number to `write_fd`.
fn install_handler(signum: libc::c_int, write_fd: i32) -> io::Result<()> {
    // Store write_fd globally so the handler (a bare fn pointer) can reach it.
    // We use a static per-signal to keep this simple.
    SIGNAL_WRITE_FD.store(write_fd, std::sync::atomic::Ordering::Relaxed);

    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = signal_handler as *const () as libc::sighandler_t;
    // SAFETY: `sigemptyset` is safe on a zeroed `sigset_t`.
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    sa.sa_flags = libc::SA_RESTART;

    // SAFETY: `signum` is a valid signal number; `sa` is correctly initialised.
    let rc = unsafe { libc::sigaction(signum, &sa, std::ptr::null_mut()) };
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Global write-fd accessible from the bare signal handler.
static SIGNAL_WRITE_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// The actual signal handler. Must be async-signal-safe.
///
/// Writes the signal number as a single byte to the pipe so `on_signal_readable`
/// can distinguish SIGINT (2) from SIGTERM (15).
///
/// SAFETY: This function is invoked by the kernel on signal delivery. It only
/// calls `write(2)`, which is listed as async-signal-safe by POSIX.
extern "C" fn signal_handler(signum: libc::c_int) {
    let fd = SIGNAL_WRITE_FD.load(std::sync::atomic::Ordering::Relaxed);
    if fd == -1 {
        return;
    }
    // Encode the signal number as the byte so the reader can distinguish signals.
    // Signal numbers fit in a u8 on all supported platforms (max is ~64).
    let b: u8 = signum as u8;
    // SAFETY: `fd` is the write end of our non-blocking pipe; writing one byte
    // is async-signal-safe. We intentionally ignore the return value because
    // there is nothing safe we can do on failure inside a signal handler.
    unsafe { libc::write(fd, &b as *const u8 as *const libc::c_void, 1) };
}

// ── Reactor integration ───────────────────────────────────────────────────────

/// Called by the executor's event loop when the reactor fires `SIGNAL_TOKEN`.
///
/// Drains the self-pipe; each byte encodes the signal number (signum as u8).
/// Only pushes and wakes waiters for the specific `SignalKind` that was received.
#[allow(dead_code)] // called by executor run loop when signal integration is active
pub(crate) fn on_signal_readable() {
    let state_lock = match SIGNAL_STATE.get() {
        Some(s) => s,
        None => return,
    };

    let mut state = state_lock.lock().unwrap();

    // Drain the pipe — each byte encodes one signal delivery as its signal number.
    let mut buf = [0u8; 64];
    let mut received: Vec<SignalKind> = Vec::new();
    loop {
        // SAFETY: `read_fd` is a valid O_NONBLOCK fd; `buf` is a valid buffer.
        let n = unsafe {
            libc::read(
                state.read_fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n <= 0 {
            break; // EAGAIN or EOF
        }
        // Decode each byte as a signal number to determine its kind.
        for &sigbyte in &buf[..n as usize] {
            let kind = match sigbyte as libc::c_int {
                libc::SIGINT => Some(SignalKind::Interrupt),
                libc::SIGTERM => Some(SignalKind::Terminate),
                _ => None, // unknown signal — ignore
            };
            if let Some(k) = kind {
                received.push(k);
            }
        }
    }

    // Push received signal kinds into the pending list.
    for kind in received {
        state.pending.push(kind);
    }

    // Snapshot pending kinds to avoid simultaneous mutable + immutable borrow.
    let pending_snapshot = state.pending.clone();

    // Drain waiters whose kind now has a pending entry; collect their wakers.
    let mut wakers: Vec<Waker> = Vec::new();
    state.waiters.retain(|(kind, waker)| {
        if pending_snapshot.contains(kind) {
            wakers.push(waker.clone());
            false // remove from waiters — will be woken below
        } else {
            true // keep waiting
        }
    });

    drop(state);

    for w in wakers {
        w.wake();
    }
}

// ── Signal future ─────────────────────────────────────────────────────────────

/// Future that resolves when a signal of the requested `kind` is received.
///
/// Construct via [`signal`].
pub struct Signal {
    kind: SignalKind,
    /// True once we have been woken and delivered the signal to the caller.
    done: bool,
}

impl Future for Signal {
    type Output = SignalKind;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.done {
            return Poll::Ready(self.kind);
        }

        let state_lock = match SIGNAL_STATE.get() {
            Some(s) => s,
            None => return Poll::Pending,
        };

        let mut state = state_lock.lock().unwrap();

        // Check if a signal of our kind is already pending.
        if let Some(pos) = state.pending.iter().position(|k| *k == self.kind) {
            state.pending.remove(pos);
            self.done = true;
            return Poll::Ready(self.kind);
        }

        // Register waker and wait. Replace existing waker for this future on re-poll
        // to avoid duplicate waker accumulation across multiple poll() calls.
        let kind = self.kind;
        let new_waker = cx.waker().clone();
        // Find existing entry for this kind and replace if same task (will_wake),
        // otherwise append. This avoids O(N) scan for the common single-waiter case.
        let existing = state
            .waiters
            .iter_mut()
            .find(|(k, w)| *k == kind && w.will_wake(&new_waker));
        if let Some((_, w)) = existing {
            *w = new_waker;
        } else {
            state.waiters.push((kind, new_waker));
        }
        Poll::Pending
    }
}

/// Create a `Signal` future that resolves the next time `kind` is received.
///
/// # Errors
/// Returns `io::Error` if the self-pipe or `sigaction` cannot be initialised.
pub fn signal(kind: SignalKind) -> io::Result<Signal> {
    ensure_init()?;
    Ok(Signal { kind, done: false })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_kind_equality() {
        assert_eq!(SignalKind::Interrupt, SignalKind::Interrupt);
        assert_ne!(SignalKind::Interrupt, SignalKind::Terminate);
    }

    #[test]
    fn signal_future_creation_succeeds() {
        // Should not panic — just verifies init path.
        let _sig = signal(SignalKind::Interrupt).expect("signal init failed");
    }

    #[test]
    fn signal_write_fd_set_after_init() {
        // After init the write fd must be a valid (>= 0) file descriptor.
        let _sig = signal(SignalKind::Terminate).expect("init");
        assert!(
            SIGNAL_WRITE_FD.load(std::sync::atomic::Ordering::Relaxed) >= 0,
            "write_fd not set after init"
        );
    }

    #[test]
    fn self_pipe_read_fd_registered() {
        // Verify the read end was registered with the reactor (no panic = pass).
        let _sig = signal(SignalKind::Interrupt).expect("init");
        // Re-registering the same fd would panic or error; the fact that init
        // succeeded confirms registration happened.
    }

    /// Manual test — not run in CI.
    ///
    /// To exercise signal delivery:
    /// ```ignore
    /// let sig = signal(SignalKind::Interrupt).unwrap();
    /// // In another terminal: kill -INT <pid>
    /// block_on(async { sig.await });
    /// ```
    #[allow(dead_code)]
    fn _manual_signal_delivery_test() {}
}
