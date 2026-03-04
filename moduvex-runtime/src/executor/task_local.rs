//! Task-local storage for async contexts.
//!
//! Provides [`TaskLocal<T>`] — a key for per-task storage, analogous to
//! `thread_local!` but scoped to an async task's execution. Values are set
//! via [`TaskLocal::scope`] and read via [`TaskLocal::with`] /
//! [`TaskLocal::try_with`].
//!
//! # Example
//! ```
//! moduvex_runtime::task_local! {
//!     static REQUEST_ID: u64;
//! }
//!
//! moduvex_runtime::block_on(async {
//!     REQUEST_ID.scope(42, async {
//!         REQUEST_ID.with(|id| assert_eq!(*id, 42));
//!     }).await;
//! });
//! ```

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

// ── Thread-local storage backend ─────────────────────────────────────────────

thread_local! {
    static STORAGE: RefCell<HashMap<usize, Box<dyn Any>>> = RefCell::new(HashMap::new());
}

// ── TaskLocal key ────────────────────────────────────────────────────────────

/// A key for task-local storage, created by the [`task_local!`] macro.
///
/// Each static `TaskLocal<T>` has a unique address used as the storage key.
pub struct TaskLocal<T: 'static> {
    _marker: PhantomData<T>,
}

impl<T: 'static> TaskLocal<T> {
    /// Internal constructor — use [`task_local!`] instead.
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self { _marker: PhantomData }
    }

    /// Address-based key for the thread-local HashMap.
    fn key(&'static self) -> usize {
        self as *const Self as usize
    }

    /// Run `future` with `value` set for this key. Restores the previous
    /// value (if any) after each poll, so other tasks on the same thread
    /// don't see stale data.
    pub fn scope<F: Future>(&'static self, value: T, future: F) -> Scope<T, F> {
        Scope {
            key: self,
            value: Some(value),
            future,
        }
    }

    /// Access the current value, panicking if no scope is active.
    pub fn with<F, R>(&'static self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        self.try_with(f)
            .expect("TaskLocal::with() called outside of a scope")
    }

    /// Access the current value, returning `Err` if no scope is active.
    pub fn try_with<F, R>(&'static self, f: F) -> Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        STORAGE.with(|s| {
            let map = s.borrow();
            match map.get(&self.key()) {
                Some(boxed) => {
                    let val = boxed.downcast_ref::<T>().expect("TaskLocal type mismatch");
                    Ok(f(val))
                }
                None => Err(AccessError),
            }
        })
    }
}

// SAFETY: TaskLocal itself holds no data — all data lives in thread-local storage.
unsafe impl<T: 'static> Sync for TaskLocal<T> {}

// ── AccessError ──────────────────────────────────────────────────────────────

/// Returned by [`TaskLocal::try_with`] when no scope is active.
#[derive(Debug)]
pub struct AccessError;

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("task-local value not set in current scope")
    }
}

// ── Scope future ─────────────────────────────────────────────────────────────

/// A future that sets a task-local value around each poll of an inner future.
///
/// Created by [`TaskLocal::scope`].
pub struct Scope<T: 'static, F: Future> {
    key: &'static TaskLocal<T>,
    /// Holds our value when the inner future is *not* being polled.
    value: Option<T>,
    future: F,
}

impl<T: 'static, F: Future> Future for Scope<T, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We only project to `future` (pinned) and `value`/`key` (Unpin).
        let this = unsafe { self.get_unchecked_mut() };

        // Enter: move our value into thread-local storage, capture previous.
        let val = this.value.take().expect("Scope polled after completion");
        let key_addr = this.key.key();
        let prev = STORAGE.with(|s| s.borrow_mut().insert(key_addr, Box::new(val)));

        // Poll the inner future.
        let inner = unsafe { Pin::new_unchecked(&mut this.future) };
        let result = inner.poll(cx);

        // Exit: take value back, restore previous.
        let current = STORAGE.with(|s| s.borrow_mut().remove(&key_addr));
        if let Some(p) = prev {
            STORAGE.with(|s| s.borrow_mut().insert(key_addr, p));
        }

        match result {
            Poll::Ready(output) => Poll::Ready(output),
            Poll::Pending => {
                // Stash value back for next poll.
                if let Some(boxed) = current {
                    this.value = Some(*boxed.downcast::<T>().expect("type mismatch"));
                }
                Poll::Pending
            }
        }
    }
}

/// Declare a task-local key.
///
/// # Example
/// ```
/// moduvex_runtime::task_local! {
///     static MY_KEY: String;
/// }
/// ```
#[macro_export]
macro_rules! task_local {
    ($(#[$attr:meta])* $vis:vis static $name:ident : $ty:ty ; $($rest:tt)*) => {
        $(#[$attr])*
        $vis static $name: $crate::executor::task_local::TaskLocal<$ty> =
            $crate::executor::task_local::TaskLocal::new();
        $crate::task_local!($($rest)*);
    };
    () => {};
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{block_on, block_on_with_spawn, spawn};

    task_local! {
        static FOO: u32;
        static BAR: String;
    }

    #[test]
    fn scope_sets_and_reads_value() {
        block_on(async {
            FOO.scope(42, async {
                FOO.with(|v| assert_eq!(*v, 42));
            }).await;
        });
    }

    #[test]
    fn try_with_returns_err_outside_scope() {
        block_on(async {
            assert!(FOO.try_with(|_| ()).is_err());
        });
    }

    #[test]
    fn nested_scopes_restore_previous() {
        block_on(async {
            FOO.scope(1, async {
                FOO.with(|v| assert_eq!(*v, 1));
                FOO.scope(2, async {
                    FOO.with(|v| assert_eq!(*v, 2));
                }).await;
                // Outer scope restored.
                FOO.with(|v| assert_eq!(*v, 1));
            }).await;
        });
    }

    #[test]
    fn multiple_keys_independent() {
        block_on(async {
            FOO.scope(99, async {
                BAR.scope(String::from("hello"), async {
                    FOO.with(|v| assert_eq!(*v, 99));
                    BAR.with(|v| assert_eq!(v, "hello"));
                }).await;
            }).await;
        });
    }

    #[test]
    fn scope_value_not_visible_after_await() {
        block_on(async {
            FOO.scope(10, async {}).await;
            assert!(FOO.try_with(|_| ()).is_err());
        });
    }

    #[test]
    fn spawned_task_does_not_inherit_parent_scope() {
        block_on_with_spawn(async {
            FOO.scope(777, async {
                let jh = spawn(async {
                    FOO.try_with(|_| ()).is_err()
                });
                assert!(jh.await.unwrap(), "spawned task should not see parent scope");
            }).await;
        });
    }
}
