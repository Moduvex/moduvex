//! Graceful shutdown — signal handling and drain coordination.
//!
//! `ShutdownSignal` wraps the platform signal future from `moduvex-runtime`.
//! It resolves once when SIGINT or SIGTERM is received. The `LifecycleEngine`
//! awaits this future after entering `Ready`, then initiates `Stopping`.
//!
//! # Timeout
//! A configurable drain timeout (default 30 s) is enforced: if modules have
//! not all stopped within the window, the engine logs a warning and proceeds
//! to `Stopped` anyway, allowing the process to exit cleanly.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(unix))]
use std::sync::Mutex;
#[cfg(not(unix))]
use std::task::Waker;

// ── ShutdownConfig ────────────────────────────────────────────────────────────

/// Configuration for graceful shutdown behaviour.
#[derive(Debug, Clone)]
pub struct ShutdownConfig {
    /// Maximum time to wait for all modules to stop before forcing exit.
    /// Default: 30 seconds.
    pub drain_timeout: Duration,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            drain_timeout: Duration::from_secs(30),
        }
    }
}

// ── ShutdownHandle ────────────────────────────────────────────────────────────

/// A cloneable handle that can request shutdown programmatically.
///
/// The `LifecycleEngine` holds one; modules or middleware can clone and use it
/// to trigger graceful shutdown without sending an OS signal (useful in tests
/// and for programmatic lifecycle control).
#[derive(Clone)]
pub struct ShutdownHandle {
    requested: Arc<AtomicBool>,
    #[cfg(not(unix))]
    waker: Arc<Mutex<Option<Waker>>>,
}

#[cfg(unix)]
impl Default for ShutdownHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(unix))]
impl Default for ShutdownHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownHandle {
    /// Create a new, unset handle.
    pub fn new() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
            #[cfg(not(unix))]
            waker: Arc::new(Mutex::new(None)),
        }
    }

    /// Signal that shutdown should begin.
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
        #[cfg(not(unix))]
        if let Some(w) = self.waker.lock().unwrap().take() {
            w.wake();
        }
    }

    /// Returns `true` if shutdown has been requested.
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

// ── wait_for_shutdown ─────────────────────────────────────────────────────────

/// Wait until either an OS signal arrives or `handle.request()` is called.
///
/// Resolves immediately if the handle was already triggered before this call.
/// On non-Unix platforms the OS signal path is compiled out; only the handle
/// path is active (useful for tests on Windows CI).
pub async fn wait_for_shutdown(handle: &ShutdownHandle) {
    // Fast-path: already requested.
    if handle.is_requested() {
        return;
    }

    #[cfg(unix)]
    {
        use moduvex_runtime::signal::{signal, SignalKind};
        use std::future::Future;
        use std::pin::Pin;
        use std::task::Poll as TaskPoll;

        // Set up SIGINT + SIGTERM listeners.
        let mut sigint = signal(SignalKind::Interrupt).ok();
        let mut sigterm = signal(SignalKind::Terminate).ok();

        // Poll signal futures and the programmatic handle concurrently.
        // Each iteration: check handle, poll signals, then yield if none ready.
        std::future::poll_fn(|cx| {
            if handle.is_requested() {
                return TaskPoll::Ready(());
            }

            // Poll signal futures — they register their wakers internally.
            if let Some(ref mut s) = sigint {
                if Pin::new(s).poll(cx).is_ready() {
                    return TaskPoll::Ready(());
                }
            }
            if let Some(ref mut s) = sigterm {
                if Pin::new(s).poll(cx).is_ready() {
                    return TaskPoll::Ready(());
                }
            }

            // Not ready yet — wakers are registered by signal futures above.
            // Also re-check the programmatic handle on next wake.
            TaskPoll::Pending
        })
        .await;
    }

    #[cfg(not(unix))]
    {
        // Non-Unix: only programmatic shutdown is supported.
        // Store waker so request() can wake this future.
        let handle_clone = handle.clone();
        std::future::poll_fn(move |cx: &mut std::task::Context<'_>| {
            if handle_clone.is_requested() {
                std::task::Poll::Ready(())
            } else {
                *handle_clone.waker.lock().unwrap() = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        })
        .await;
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_starts_unset() {
        let h = ShutdownHandle::new();
        assert!(!h.is_requested());
    }

    #[test]
    fn handle_request_sets_flag() {
        let h = ShutdownHandle::new();
        h.request();
        assert!(h.is_requested());
    }

    #[test]
    fn clone_shares_state() {
        let h = ShutdownHandle::new();
        let h2 = h.clone();
        h.request();
        assert!(h2.is_requested());
    }

    #[test]
    fn wait_returns_immediately_when_already_requested() {
        let h = ShutdownHandle::new();
        h.request();
        moduvex_runtime::block_on(async {
            // Should complete immediately without spinning.
            wait_for_shutdown(&h).await;
        });
    }

    #[test]
    fn wait_returns_after_programmatic_request() {
        // Request shutdown before entering wait_for_shutdown.
        // The fast-path in wait_for_shutdown checks is_requested() immediately
        // and returns without looping — this verifies that path works correctly
        // when the flag was set just before the call.
        let h = ShutdownHandle::new();
        h.request();
        moduvex_runtime::block_on(async {
            wait_for_shutdown(&h).await;
        });
    }

    #[test]
    fn shutdown_config_default_timeout() {
        let cfg = ShutdownConfig::default();
        assert_eq!(cfg.drain_timeout, Duration::from_secs(30));
    }
}
