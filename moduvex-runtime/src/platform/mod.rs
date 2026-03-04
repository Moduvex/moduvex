//! Platform abstraction layer.
//!
//! Selects the appropriate OS-level primitives at compile time and re-exports
//! them through a unified `sys` surface that the rest of the crate uses.
//!
//! Supported targets:
//! - Linux   → epoll (via `reactor::epoll`)
//! - macOS   → kqueue (via `reactor::kqueue`)
//! - FreeBSD → kqueue (via `reactor::kqueue`)
//! - Windows → IOCP stub (via `reactor::iocp`)

pub mod sys;

// Re-export the most-used types at the platform level so callers can write
// `platform::RawSource` instead of `platform::sys::RawSource`.
pub use sys::{Event, Events, Interest, RawSource};
pub use sys::{close_fd, create_pipe, events_with_capacity, set_nonblocking};
