//! Runtime builder and handle.
//!
//! Provides [`RuntimeBuilder`] for configuring and constructing a [`Runtime`],
//! and [`Runtime`] for driving async entry points.
//!
//! # Example
//! ```
//! use moduvex_runtime::Runtime;
//!
//! let rt = Runtime::builder()
//!     .thread_per_core()
//!     .enable_io()
//!     .enable_time()
//!     .build()
//!     .unwrap();
//!
//! rt.block_on(async { 1 + 1 });
//! ```
//!
//! # Multi-threaded example
//! ```
//! use moduvex_runtime::Runtime;
//!
//! let rt = Runtime::builder()
//!     .worker_threads(4)
//!     .build()
//!     .unwrap();
//!
//! rt.block_on(async { 1 + 1 });
//! ```

use std::future::Future;

use crate::executor;

// ── RuntimeBuilder ───────────────────────────────────────────────────────────

/// Configures a [`Runtime`].
///
/// Defaults to single-threaded (1 worker). Call [`worker_threads`] to opt into
/// multi-threaded work-stealing mode.
///
/// [`worker_threads`]: RuntimeBuilder::worker_threads
pub struct RuntimeBuilder {
    /// Number of worker threads. 1 = single-threaded (default).
    num_workers: usize,
}

impl RuntimeBuilder {
    fn new() -> Self {
        Self { num_workers: 1 }
    }

    /// Use thread-per-core threading model (single-threaded, default).
    pub fn thread_per_core(self) -> Self {
        Self { num_workers: 1 }
    }

    /// Set the number of worker threads for multi-threaded work-stealing mode.
    ///
    /// - `n = 1`: single-threaded (same as default)
    /// - `n > 1`: spawns N-1 background threads; main thread is worker 0
    ///
    /// # Panics
    /// Panics if `n == 0`.
    pub fn worker_threads(self, n: usize) -> Self {
        assert!(n > 0, "worker_threads must be at least 1");
        Self { num_workers: n }
    }

    /// Enable the I/O reactor (no-op: always enabled).
    pub fn enable_io(self) -> Self {
        self
    }

    /// Enable the timer wheel (no-op: always enabled).
    pub fn enable_time(self) -> Self {
        self
    }

    /// Build the runtime.
    pub fn build(self) -> std::io::Result<Runtime> {
        Ok(Runtime {
            num_workers: self.num_workers,
        })
    }
}

// ── Runtime ──────────────────────────────────────────────────────────────────

/// A configured async runtime.
///
/// Created via [`Runtime::builder`]. Drives futures to completion with
/// [`Runtime::block_on`].
pub struct Runtime {
    /// Number of worker threads (1 = single-threaded).
    num_workers: usize,
}

impl Runtime {
    /// Create a new [`RuntimeBuilder`].
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// Drive `future` to completion, with `spawn()` available inside.
    ///
    /// Uses single-threaded mode when `num_workers == 1` (default), or
    /// multi-threaded work-stealing when `num_workers > 1`.
    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        if self.num_workers <= 1 {
            executor::block_on_with_spawn(future)
        } else {
            executor::block_on_multi(future, self.num_workers)
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_runtime() {
        let rt = Runtime::builder()
            .thread_per_core()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let v = rt.block_on(async { 42u32 });
        assert_eq!(v, 42);
    }

    #[test]
    fn runtime_spawn_works() {
        let rt = Runtime::builder().build().unwrap();
        let result = rt.block_on(async {
            let jh = crate::spawn(async { 100u32 });
            jh.await.unwrap()
        });
        assert_eq!(result, 100);
    }

    #[test]
    fn runtime_worker_threads_api() {
        let rt = Runtime::builder()
            .worker_threads(2)
            .build()
            .unwrap();
        let v = rt.block_on(async { 7u32 });
        assert_eq!(v, 7);
    }

    #[test]
    fn runtime_multi_thread_spawn() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let rt = Runtime::builder().worker_threads(4).build().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();

        rt.block_on(async move {
            let mut handles = Vec::new();
            for _ in 0..50 {
                let cc = c.clone();
                handles.push(crate::spawn(async move {
                    cc.fetch_add(1, Ordering::SeqCst);
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });

        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 50);
    }

    #[test]
    #[should_panic(expected = "worker_threads must be at least 1")]
    fn runtime_zero_workers_panics() {
        Runtime::builder().worker_threads(0).build().unwrap();
    }
}
