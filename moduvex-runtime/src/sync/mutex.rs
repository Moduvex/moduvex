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
        LockFuture {
            inner: &self.inner,
            registered_waker: None,
        }
    }
}

// ── LockFuture ────────────────────────────────────────────────────────────────

/// Future returned by [`Mutex::lock`].
///
/// Stores its registered waker so it can remove itself from the queue on
/// cancellation (drop before completion). This prevents MutexGuard::drop from
/// waking an already-dropped task.
pub struct LockFuture<'a, T> {
    inner: &'a Arc<StdMutex<Inner<T>>>,
    /// The waker we pushed into `waiters`, stored so Drop can remove it.
    /// `None` if we have not yet registered (or have already been resolved).
    registered_waker: Option<Waker>,
}

impl<T> Future for LockFuture<'_, T> {
    type Output = MutexGuard<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut g = self.inner.lock().unwrap();
        if !g.locked {
            g.locked = true;
            self.registered_waker = None; // lock acquired; no longer in waiter queue
            let value_ptr = g.value.get();
            Poll::Ready(MutexGuard {
                inner: Arc::clone(self.inner),
                value_ptr,
            })
        } else {
            let new_waker = cx.waker().clone();
            if let Some(ref existing) = self.registered_waker {
                // Already registered: update in place if waker changed.
                if !existing.will_wake(&new_waker) {
                    // Replace our stale waker in the queue with the new one.
                    for w in &mut g.waiters {
                        if w.will_wake(existing) {
                            *w = new_waker.clone();
                            break;
                        }
                    }
                    self.registered_waker = Some(new_waker);
                }
            } else {
                // First time blocked — push waker and remember it for cleanup.
                g.waiters.push_back(new_waker.clone());
                self.registered_waker = Some(new_waker);
            }
            Poll::Pending
        }
    }
}

impl<T> Drop for LockFuture<'_, T> {
    fn drop(&mut self) {
        if let Some(ref waker) = self.registered_waker {
            // Remove our waker so MutexGuard::drop doesn't wake a dead task.
            if let Ok(mut g) = self.inner.lock() {
                // Remove the first waker in the queue that matches ours.
                if let Some(pos) = g.waiters.iter().position(|w| w.will_wake(waker)) {
                    g.waiters.remove(pos);
                }
            }
        }
    }
}

// ── MutexGuard ────────────────────────────────────────────────────────────────

/// RAII guard that releases the async lock on drop and wakes the next waiter.
pub struct MutexGuard<T> {
    inner: Arc<StdMutex<Inner<T>>>,
    /// Cached raw pointer to the protected value. Avoids acquiring the
    /// StdMutex on every deref. Valid for the lifetime of this guard because:
    /// - The Arc keeps the Inner allocation alive.
    /// - The async `locked` flag prevents concurrent mutation.
    value_ptr: *mut T,
}

// SAFETY: MutexGuard<T> is Send+Sync when T: Send because:
// - The async lock serialises all access to the value.
// - The raw pointer comes from UnsafeCell inside an Arc (heap-stable).
unsafe impl<T: Send> Send for MutexGuard<T> {}
unsafe impl<T: Send> Sync for MutexGuard<T> {}

impl<T> Deref for MutexGuard<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: we hold the async lock (`locked == true`), so no other
        // `MutexGuard` exists concurrently. The Arc keeps memory alive.
        // `value_ptr` was obtained at lock acquisition time.
        unsafe { &*self.value_ptr }
    }
}

impl<T> DerefMut for MutexGuard<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the async lock exclusively; `&mut self` ensures
        // no aliased mutable references exist via this guard.
        unsafe { &mut *self.value_ptr }
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
    use crate::executor::{block_on, block_on_with_spawn, spawn};
    use std::sync::Arc as StdArc;

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

    // ── Additional mutex tests ─────────────────────────────────────────────

    #[test]
    fn mutex_stress_100_concurrent_increments() {
        let counter = StdArc::new(Mutex::new(0u64));
        let c = counter.clone();
        block_on_with_spawn(async move {
            let mut handles = Vec::new();
            for _ in 0..100 {
                let cc = c.clone();
                handles.push(spawn(async move {
                    let mut g = cc.lock().await;
                    *g += 1;
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });
        let final_val = block_on(async { *counter.lock().await });
        assert_eq!(final_val, 100);
    }

    #[test]
    fn mutex_fifo_all_entries_recorded() {
        // All lockers queue; each pushes a known value.
        let order = StdArc::new(Mutex::new(Vec::<u32>::new()));
        let o = order.clone();
        block_on_with_spawn(async move {
            let mut handles = Vec::new();
            for i in 0u32..5 {
                let oo = o.clone();
                handles.push(spawn(async move {
                    let mut g = oo.lock().await;
                    g.push(i);
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });
        let v = block_on(async { order.lock().await.len() });
        assert_eq!(v, 5);
    }

    #[test]
    fn mutex_guard_deref() {
        block_on(async {
            let m = Mutex::new(vec![1u32, 2, 3]);
            let g = m.lock().await;
            assert_eq!(g.len(), 3);
            assert_eq!((*g)[1], 2);
        });
    }

    #[test]
    fn mutex_guard_deref_mut() {
        block_on(async {
            let m = Mutex::new(0u32);
            let mut g = m.lock().await;
            *g = 99;
            drop(g);
            assert_eq!(*m.lock().await, 99);
        });
    }

    #[test]
    fn mutex_reentrant_after_abort_no_deadlock() {
        block_on_with_spawn(async {
            let m = StdArc::new(Mutex::new(0u32));
            let m2 = m.clone();
            // Hold the lock in one task
            let guard = m.lock().await;
            // Spawn a task that will block trying to acquire
            let jh = spawn(async move {
                // This will be Pending because guard holds the lock
                let _ = m2.lock().await;
            });
            // Abort the waiting task
            jh.abort();
            drop(guard); // release lock — should not deadlock
            // Verify we can still acquire the lock
            *m.lock().await += 1;
            assert_eq!(*m.lock().await, 1);
        });
    }

    #[test]
    fn mutex_initial_value_preserved() {
        block_on(async {
            let m = Mutex::new(String::from("initial"));
            let g = m.lock().await;
            assert_eq!(*g, "initial");
        });
    }

    #[test]
    fn mutex_multiple_sequential_mutations() {
        block_on(async {
            let m = Mutex::new(0u32);
            for i in 1..=10u32 {
                *m.lock().await = i;
            }
            assert_eq!(*m.lock().await, 10);
        });
    }

    #[test]
    fn mutex_string_value() {
        block_on(async {
            let m = Mutex::new(String::new());
            for i in 0..5 {
                m.lock().await.push_str(&i.to_string());
            }
            assert_eq!(*m.lock().await, "01234");
        });
    }

    #[test]
    fn mutex_vec_value_append() {
        block_on(async {
            let m = Mutex::new(Vec::<u32>::new());
            for i in 0..5u32 {
                m.lock().await.push(i);
            }
            let g = m.lock().await;
            assert_eq!(*g, vec![0, 1, 2, 3, 4]);
        });
    }

    #[test]
    fn mutex_concurrent_10_tasks() {
        let counter = StdArc::new(Mutex::new(0u32));
        let c = counter.clone();
        block_on_with_spawn(async move {
            let mut handles = Vec::new();
            for _ in 0..10 {
                let cc = c.clone();
                handles.push(spawn(async move {
                    *cc.lock().await += 1;
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });
        let v = block_on(async { *counter.lock().await });
        assert_eq!(v, 10);
    }

    #[test]
    fn mutex_new_value_is_accessible() {
        block_on(async {
            let m = Mutex::new(42u64);
            assert_eq!(*m.lock().await, 42);
        });
    }

    #[test]
    fn mutex_lock_after_multiple_releases() {
        block_on(async {
            let m = Mutex::new(0u32);
            for _ in 0..5 {
                let mut g = m.lock().await;
                *g += 1;
                drop(g);
            }
            assert_eq!(*m.lock().await, 5);
        });
    }

    #[test]
    fn mutex_guard_cannot_alias() {
        // Taking a second lock while guard is held blocks (we verify by spawning)
        let m = StdArc::new(Mutex::new(0u32));
        let m2 = m.clone();
        block_on_with_spawn(async move {
            let g = m.lock().await;
            let jh = spawn(async move {
                // This should block until g is dropped
                *m2.lock().await += 1;
            });
            // Release g after spawning
            drop(g);
            jh.await.unwrap();
            assert_eq!(*m.lock().await, 1);
        });
    }

    #[test]
    fn mutex_hashmap_value() {
        block_on(async {
            use std::collections::HashMap;
            let m = Mutex::new(HashMap::<String, u32>::new());
            m.lock().await.insert("a".to_string(), 1);
            m.lock().await.insert("b".to_string(), 2);
            let g = m.lock().await;
            assert_eq!(g.len(), 2);
            assert_eq!(g.get("a"), Some(&1));
        });
    }

    #[test]
    fn mutex_u64_max_value() {
        block_on(async {
            let m = Mutex::new(u64::MAX);
            assert_eq!(*m.lock().await, u64::MAX);
        });
    }

    #[test]
    fn mutex_wraps_arc() {
        block_on(async {
            let inner = StdArc::new(0u32);
            let m = Mutex::new(inner.clone());
            let g = m.lock().await;
            assert_eq!(StdArc::strong_count(&*g), 2); // inner + guard's ref
        });
    }

    #[test]
    fn mutex_lock_and_immediately_drop() {
        block_on(async {
            let m = Mutex::new(42u32);
            drop(m.lock().await); // lock and release immediately
            // Verify we can lock again
            assert_eq!(*m.lock().await, 42);
        });
    }

    #[test]
    fn mutex_20_concurrent_tasks() {
        let counter = StdArc::new(Mutex::new(0u32));
        let c = counter.clone();
        block_on_with_spawn(async move {
            let handles: Vec<_> = (0..20)
                .map(|_| {
                    let cc = c.clone();
                    spawn(async move { *cc.lock().await += 1 })
                })
                .collect();
            for h in handles {
                h.await.unwrap();
            }
        });
        let v = block_on(async { *counter.lock().await });
        assert_eq!(v, 20);
    }
}
