//! Signal handling for async tasks.
//!
//! Provides a `Signal` future that resolves when the requested OS signal is
//! delivered to the process.
//!
//! # Platform support
//! - **Unix** (Linux, macOS, FreeBSD): fully implemented via the self-pipe
//!   trick + `libc::sigaction`. See [`unix_signal`] for details.
//! - **Windows**: stub — `todo!("SetConsoleCtrlHandler")`.
//!
//! # Example
//! ```no_run
//! use moduvex_runtime::signal::{signal, SignalKind};
//! use moduvex_runtime::block_on;
//!
//! block_on(async {
//!     let sig = signal(SignalKind::Interrupt).expect("signal init");
//!     println!("waiting for Ctrl-C…");
//!     sig.await;
//!     println!("received SIGINT");
//! });
//! ```

// ── Platform dispatch ─────────────────────────────────────────────────────────

#[cfg(unix)]
pub mod unix_signal;

#[cfg(unix)]
pub use unix_signal::{signal, Signal, SignalKind};

// `SIGNAL_TOKEN` and `on_signal_readable` are `pub(crate)` in `unix_signal`
// and will be used by the executor run loop when signal integration is wired
// in. They are accessed directly as `unix_signal::SIGNAL_TOKEN` etc. from
// within the crate.
#[cfg(unix)]
#[allow(unused_imports)]
pub(crate) use unix_signal::{on_signal_readable, SIGNAL_TOKEN};

// ── Windows stub ──────────────────────────────────────────────────────────────

#[cfg(windows)]
mod windows_signal {
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// The kind of signal received (Windows stub).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SignalKind {
        /// Ctrl-C (maps to SIGINT on Unix).
        Interrupt,
        /// Ctrl-Break / shutdown (maps to SIGTERM on Unix).
        Terminate,
    }

    /// Signal future stub for Windows.
    pub struct Signal {
        kind: SignalKind,
    }

    impl Future for Signal {
        type Output = SignalKind;
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    /// Create a `Signal` future (Windows — not yet implemented).
    pub fn signal(kind: SignalKind) -> io::Result<Signal> {
        Ok(Signal { kind })
    }
}

#[cfg(windows)]
pub use windows_signal::{signal, Signal, SignalKind};
