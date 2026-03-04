//! Async mutex — cooperative mutual exclusion for async tasks.
//!
//! Unlike `std::sync::Mutex`, locking suspends the calling task (yields back
//! to the executor) instead of blocking the OS thread. This is critical inside
//! async contexts where blocking would starve other tasks sharing the thread.
//!
//! # Design
//! - Inner value protected by a `std::sync::Mutex` for the critical section
//!   of updating waker queues and the locked flag.
//! - A `VecDeque<Waker>` wait queue ensures FIFO fairness across contenders.
//! - `MutexGuard` drops the lock and wakes the next waiter on `Drop`.

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll, Waker};

// ── Inner state ───────────────────────────────────────────────────────────────

struct Inner<T> {
    /// Whether the async lock is currently held by a `MutexGuard`.
    locked: bool,
    /// Tasks waiting to acquire the lock, in arrival order (FIFO).
    waiters: VecDeque<Waker>,
    /// The protected value.
    ///
    /// `UnsafeCell` allows mutation through a shared `Arc<Inner<T>>`.
    /// Safe because access is serialised: only the current `MutexGuard`
    /// holder may dereference this pointer, and there is at most one guard
    /// alive at a time (enforced by `locked`).
    value: UnsafeCell<T>,
}

// SAFETY: `Mutex<T>` must be `Send + Sync` when `T: Send` so it can be shared
// across async tasks. The `UnsafeCell` is safe because mutation is serialised
// by the `locked` flag inside the `StdMutex<Inner>`.
unsafe impl<T: Send> Send for Inner<T> {}
unsafe impl<T: Send> Sync for Inner<T> {}

// ── Mutex ─────────────────────────────────────────────────────────────────────

/// Async-aware mutual exclusion primitive.
///
/// Wraps a value of type `T`; concurrent tasks suspend (not block) while
/// waiting for the lock.
pub struct Mutex<T> {
    inner: Arc<StdMutex<Inner<T>>>,
}

impl<T> Mutex<T> {
    /// Create a new `Mutex` wrapping `value`.
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(Inner {
                locked: false,
                waiters: VecDeque::new(),
                value: UnsafeCell::new(value),
            })),
        }
    }

    /// Acquire the lock asynchronously, returning a `MutexGuard<T>`.
    ///
    /// The returned future suspends if the lock is already held and resumes
    /// once the previous holder's `MutexGuard` is dropped.
    pub fn lock(&self) -> LockFuture<'_, T> {
        LockFuture { inner: &self.inner }
    }
}

// ── LockFuture ────────────────────────────────────────────────────────────────

/// Future returned by [`Mutex::lock`].
pub struct LockFuture<'a, T> {
    inner: &'a Arc<StdMutex<Inner<T>>>,
}

impl<T> Future for LockFuture<'_, T> {
    type Output = MutexGuard<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut g = self.inner.lock().unwrap();
        if !g.locked {
            g.locked = true;
            Poll::Ready(MutexGuard { inner: Arc::clone(self.inner) })
        } else {
            g.waiters.push_back(cx.waker().clone());
            Poll::Pending
        }
    }
}

// ── MutexGuard ────────────────────────────────────────────────────────────────

/// RAII guard that releases the async lock on drop and wakes the next waiter.
pub struct MutexGuard<T> {
    inner: Arc<StdMutex<Inner<T>>>,
}

impl<T> Deref for MutexGuard<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: we hold the async lock (`locked == true`), so no other
        // `MutexGuard` exists concurrently. The reference lifetime is bounded
        // by `&self` which keeps the guard (and thus the lock) alive.
        unsafe { &*self.inner.lock().unwrap().value.get() }
    }
}

impl<T> DerefMut for MutexGuard<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the async lock exclusively; `&mut self` ensures
        // no aliased mutable references exist via this guard.
        unsafe { &mut *self.inner.lock().unwrap().value.get() }
    }
}

impl<T> Drop for MutexGuard<T> {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        // Release the lock and wake the next waiter, if any.
        g.locked = false;
        if let Some(w) = g.waiters.pop_front() {
            drop(g); // release inner mutex before waking
            w.wake();
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;
    use crate::executor::{block_on, block_on_with_spawn, spawn};

    #[test]
    fn lock_and_mutate() {
        block_on(async {
            let m = Mutex::new(0u32);
            {
                let mut g = m.lock().await;
                *g += 1;
            }
            {
                let g = m.lock().await;
                assert_eq!(*g, 1);
            }
        });
    }

    #[test]
    fn sequential_locks_in_single_task() {
        block_on(async {
            let m = Mutex::new(Vec::<u32>::new());
            for i in 0..5 {
                m.lock().await.push(i);
            }
            let g = m.lock().await;
            assert_eq!(*g, vec![0, 1, 2, 3, 4]);
        });
    }

    #[test]
    fn concurrent_lock_via_spawn() {
        let counter = StdArc::new(Mutex::new(0u32));
        let c1 = counter.clone();
        let c2 = counter.clone();

        block_on_with_spawn(async move {
            let jh1 = spawn(async move {
                let mut g = c1.lock().await;
                *g += 1;
            });
            let jh2 = spawn(async move {
                let mut g = c2.lock().await;
                *g += 1;
            });
            jh1.await.unwrap();
            jh2.await.unwrap();
        });

        // Run a fresh block_on to read the result.
        let final_val = block_on(async { *counter.lock().await });
        assert_eq!(final_val, 2);
    }

    #[test]
    fn guard_drops_release_lock() {
        block_on(async {
            let m = Mutex::new(42u32);
            let g = m.lock().await;
            assert_eq!(*g, 42);
            drop(g);
            // After drop we must be able to lock again immediately.
            let g2 = m.lock().await;
            assert_eq!(*g2, 42);
        });
    }
}
