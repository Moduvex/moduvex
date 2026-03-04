//! moduvex-runtime — Custom async runtime for the Moduvex framework.
//!
//! Provides a cross-platform async runtime with:
//! - Platform-native I/O (epoll, kqueue, IOCP)
//! - Hybrid threading (thread-per-core default, opt-in work-stealing)
//! - Hierarchical timer wheel
//! - Async networking (TCP/UDP)
//! - Synchronization primitives (mpsc, oneshot, mutex)
//! - Signal handling
//! - Task-local storage

pub mod platform;
pub mod reactor;
pub mod executor;
pub mod time;
pub mod net;
pub mod sync;
pub mod signal;
pub mod runtime;

// ── Core re-exports ──────────────────────────────────────────────────────────

pub use executor::task::{JoinHandle, JoinError};
pub use executor::task_local::{TaskLocal, AccessError};
pub use runtime::{Runtime, RuntimeBuilder};

// ── Networking re-exports ────────────────────────────────────────────────────

pub use net::{AsyncRead, AsyncWrite};
pub use net::{TcpListener, TcpStream, UdpSocket};

// ── Sync re-exports ──────────────────────────────────────────────────────────

pub use sync::{Mutex, MutexGuard};

// ── Top-level convenience functions ──────────────────────────────────────────

/// Drive `future` to completion on the current thread, returning its output.
///
/// Creates a fresh single-threaded executor and runs the event loop until
/// the future resolves. Suitable for the top-level entry point of an async
/// program.
///
/// # Example
/// ```
/// let result = moduvex_runtime::block_on(async { 42u32 });
/// assert_eq!(result, 42);
/// ```
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    executor::block_on(future)
}

/// Drive `future` to completion, with `spawn` available inside the context.
///
/// Like `block_on` but registers the executor as the thread-local so that
/// `spawn()` works within the future's async context.
pub fn block_on_with_spawn<F: std::future::Future>(future: F) -> F::Output {
    executor::block_on_with_spawn(future)
}

/// Spawn a future onto the current thread's executor.
///
/// Must be called from within a `block_on_with_spawn` or `Runtime::block_on`
/// context.
///
/// # Panics
/// Panics if called outside of an executor context.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + 'static,
    F::Output: Send + 'static,
{
    executor::spawn(future)
}

/// Sleep for the given duration.
///
/// # Example
/// ```no_run
/// use std::time::Duration;
/// moduvex_runtime::block_on(async {
///     moduvex_runtime::sleep(Duration::from_millis(100)).await;
/// });
/// ```
pub async fn sleep(duration: std::time::Duration) {
    time::sleep::sleep(duration).await;
}

/// Create a periodic interval timer.
pub fn interval(period: std::time::Duration) -> time::interval::Interval {
    time::interval::interval(period)
}
