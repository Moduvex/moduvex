//! Synchronisation primitives for async tasks.
//!
//! All primitives are async-aware: waiting tasks yield back to the executor
//! instead of blocking the OS thread.
//!
//! | Primitive | Module  | Description                                      |
//! |-----------|---------|--------------------------------------------------|
//! | `mpsc`    | [`mpsc`] | Multi-producer single-consumer channel (bounded + unbounded) |
//! | `oneshot` | [`oneshot`] | Send exactly one value; `Receiver` is a `Future` |
//! | `Mutex`   | [`mutex`] | Async mutex with FIFO waiter queue               |

pub mod mpsc;
pub mod oneshot;
pub mod mutex;

pub use mutex::{Mutex, MutexGuard};
pub use oneshot::RecvError;
