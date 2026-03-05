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

use std::future::Future;

use crate::executor;

// ── RuntimeBuilder ───────────────────────────────────────────────────────────

/// Configures a [`Runtime`].
///
/// Currently single-threaded (thread-per-core). Multi-thread work-stealing
/// will be added in a future phase.
///
/// The runtime always enables both I/O and timer support — separate feature
/// flags are not meaningful for the current single-threaded implementation.
pub struct RuntimeBuilder;

impl RuntimeBuilder {
    fn new() -> Self {
        Self
    }

    /// Use thread-per-core threading model (default).
    pub fn thread_per_core(self) -> Self {
        self // already the default
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
        Ok(Runtime { _private: () })
    }
}

// ── Runtime ──────────────────────────────────────────────────────────────────

/// A configured async runtime.
///
/// Created via [`Runtime::builder`]. Drives futures to completion with
/// [`Runtime::block_on`].
pub struct Runtime {
    _private: (),
}

impl Runtime {
    /// Create a new [`RuntimeBuilder`].
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// Drive `future` to completion, with `spawn()` available inside.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        executor::block_on_with_spawn(future)
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
}
