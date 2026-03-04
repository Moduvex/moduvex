//! Task lifecycle types: `TaskHeader`, `Task`, `JoinHandle`.
//!
//! # Memory Model
//!
//! Two separate heap allocations per spawned future:
//!
//! 1. `Arc<TaskHeader>` — shared between executor (`Task`), all `Waker`s,
//!    and `JoinHandle`. Contains the atomic state, vtable pointer, join-waker
//!    slot, and the output slot (written on completion, read by JoinHandle).
//!
//! 2. `Box<TaskBody<F>>` (stored as `body_ptr: *mut ()` in `TaskHeader`) —
//!    owns the erased `Pin<Box<F>>` (the live future). Freed by the executor
//!    the moment the future resolves or the task is cancelled, independent of
//!    when the JoinHandle reads the output.
//!
//! Separating the output from the body lets `drop_body` free the future
//! immediately on completion while the output lives safely in the Arc until
//! `JoinHandle::poll` retrieves it.

use std::any::Any;
use std::cell::UnsafeCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

// ── State constants ───────────────────────────────────────────────────────────

pub(crate) const STATE_IDLE: u32 = 0;
pub(crate) const STATE_SCHEDULED: u32 = 1;
pub(crate) const STATE_RUNNING: u32 = 2;
pub(crate) const STATE_COMPLETED: u32 = 3;
pub(crate) const STATE_CANCELLED: u32 = 4;

// ── JoinError ─────────────────────────────────────────────────────────────────

/// Error returned by a `JoinHandle` when the task does not complete normally.
#[derive(Debug)]
pub enum JoinError {
    /// Task was aborted via `JoinHandle::abort()`.
    Cancelled,
    /// Task's future panicked. Panic payload preserved.
    Panic(Box<dyn Any + Send + 'static>),
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Cancelled => write!(f, "task was cancelled"),
            JoinError::Panic(_) => write!(f, "task panicked"),
        }
    }
}
impl std::error::Error for JoinError {}

// ── TaskVtable ────────────────────────────────────────────────────────────────

/// Type-erased function pointers for a concrete `TaskBody<F>`.
pub(crate) struct TaskVtable {
    /// Poll the future once. Returns `true` when the future completed (Ready).
    /// On Ready the output has been written to `TaskHeader.output`.
    pub poll: unsafe fn(body: *mut (), header: &TaskHeader, cx: &mut Context<'_>) -> bool,

    /// Free the `Box<TaskBody<F>>` allocation (future only; output lives in header).
    pub drop_body: unsafe fn(body: *mut ()),
}

// ── TaskBody ──────────────────────────────────────────────────────────────────

/// Heap allocation that owns the erased future.
struct TaskBody<F> {
    future: Pin<Box<F>>,
}

// ── Vtable implementations ────────────────────────────────────────────────────

unsafe fn body_poll<F, T>(body_ptr: *mut (), header: &TaskHeader, cx: &mut Context<'_>) -> bool
where
    F: Future<Output = T>,
    T: Send + 'static,
{
    // SAFETY: `body_ptr` is `Box::into_raw(Box<TaskBody<F>>)` cast to `*mut ()`.
    let body = &mut *(body_ptr as *mut TaskBody<F>);
    match body.future.as_mut().poll(cx) {
        Poll::Ready(val) => {
            // Store the boxed output into the header's output slot.
            // SAFETY: state=RUNNING — only this call site writes `output`.
            *header.output.get() = Some(Box::new(val) as Box<dyn Any + Send>);
            true
        }
        Poll::Pending => false,
    }
}

unsafe fn body_drop<F>(ptr: *mut ()) {
    // SAFETY: `ptr` is `Box::into_raw(Box<TaskBody<F>>)`.
    drop(Box::from_raw(ptr as *mut TaskBody<F>));
}

fn make_vtable<F, T>() -> &'static TaskVtable
where
    F: Future<Output = T>,
    T: Send + 'static,
{
    &TaskVtable {
        poll: body_poll::<F, T>,
        drop_body: body_drop::<F>,
    }
}

// ── TaskHeader ────────────────────────────────────────────────────────────────

/// Shared, reference-counted task descriptor.
///
/// Lives inside an `Arc<TaskHeader>`. Every `Waker`, the executor's `Task`,
/// and the user's `JoinHandle` all hold a clone of this Arc.
pub(crate) struct TaskHeader {
    /// Lifecycle state — see `STATE_*` constants.
    pub state: AtomicU32,

    /// Type-erased vtable for the concrete `F` / `T` types.
    pub vtable: &'static TaskVtable,

    /// Waker registered by `JoinHandle::poll`. Called when the task finishes.
    ///
    /// # Safety invariant
    /// Written only when `state < STATE_COMPLETED` (by `JoinHandle::poll` on
    /// the executor thread). Read+cleared only when transitioning to
    /// COMPLETED/CANCELLED (by `Task::poll_task` / `Task::cancel`, also on the
    /// executor thread). Single-threaded executor guarantees no data race.
    pub join_waker: UnsafeCell<Option<Waker>>,

    /// Raw pointer to the `Box<TaskBody<F>>` allocation.
    ///
    /// # Safety invariant
    /// Non-null from `Task::new` until `drop_body` is called by either
    /// `poll_task` (on completion) or `cancel`. Nulled immediately after.
    /// Only read/written while `state == STATE_RUNNING` or during cancellation.
    pub body_ptr: UnsafeCell<*mut ()>,

    /// Output value written by the vtable's `poll` on completion.
    /// Read (and taken) exactly once by `JoinHandle::poll`.
    ///
    /// # Safety invariant
    /// Written when `state` transitions to COMPLETED. Read when `state` is
    /// observed as COMPLETED by `JoinHandle::poll`. Single-threaded executor
    /// prevents concurrent writes+reads.
    pub output: UnsafeCell<Option<Box<dyn Any + Send>>>,
}

// SAFETY: All `UnsafeCell` fields in `TaskHeader` are protected by the
// atomic `state` field and the single-threaded executor invariant.
// No two threads access mutable fields concurrently.
unsafe impl Send for TaskHeader {}
unsafe impl Sync for TaskHeader {}

// ── Task ──────────────────────────────────────────────────────────────────────

/// Executor-owned handle to a spawned task.
pub(crate) struct Task {
    pub(crate) header: Arc<TaskHeader>,
}

impl Task {
    /// Allocate a new task returning the executor `Task` + user `JoinHandle<T>`.
    pub(crate) fn new<F, T>(future: F) -> (Task, JoinHandle<T>)
    where
        F: Future<Output = T> + 'static,
        T: Send + 'static,
    {
        // Allocate and leak the future body (freed via vtable.drop_body).
        let body: Box<TaskBody<F>> = Box::new(TaskBody {
            future: Box::pin(future),
        });
        let body_ptr = Box::into_raw(body) as *mut ();

        let header = Arc::new(TaskHeader {
            state: AtomicU32::new(STATE_SCHEDULED),
            vtable: make_vtable::<F, T>(),
            join_waker: UnsafeCell::new(None),
            body_ptr: UnsafeCell::new(body_ptr),
            output: UnsafeCell::new(None),
        });

        let join_arc = Arc::clone(&header);
        let task = Task { header };
        let jh = JoinHandle {
            header: join_arc,
            _marker: std::marker::PhantomData,
        };
        (task, jh)
    }

    /// Poll the task's future once. Returns `true` when the future completed.
    ///
    /// State transitions: SCHEDULED → RUNNING → IDLE (Pending) | COMPLETED (Ready)
    pub(crate) fn poll_task(&self, cx: &mut Context<'_>) -> bool {
        let h = &self.header;
        h.state.store(STATE_RUNNING, Ordering::Release);

        // SAFETY: state=RUNNING — exclusive access to body_ptr.
        let body_ptr = unsafe { *h.body_ptr.get() };
        debug_assert!(!body_ptr.is_null(), "poll_task called on freed body");

        // SAFETY: vtable matches the concrete types used in `new`.
        let completed = unsafe { (h.vtable.poll)(body_ptr, h, cx) };

        if completed {
            // Free the future body — output is now in h.output.
            // SAFETY: body_ptr valid; state=RUNNING prevents concurrent access.
            unsafe {
                (h.vtable.drop_body)(body_ptr);
                *h.body_ptr.get() = std::ptr::null_mut();
            }
            h.state.store(STATE_COMPLETED, Ordering::Release);
            // Wake the JoinHandle waiter.
            // SAFETY: state=COMPLETED — no concurrent join_waker writes.
            let waker = unsafe { (*h.join_waker.get()).take() };
            if let Some(w) = waker {
                w.wake();
            }
        } else {
            h.state.store(STATE_IDLE, Ordering::Release);
        }
        completed
    }

    /// Cancel the task: drop the future body and wake the JoinHandle.
    ///
    /// Must be called at most once by the executor.
    pub(crate) fn cancel(self) {
        let h = &self.header;
        // SAFETY: executor guarantees cancel is called while holding the Task,
        // which means state is SCHEDULED or CANCELLED (set by abort()).
        // Either way we own exclusive access to body_ptr.
        let body_ptr = unsafe { *h.body_ptr.get() };
        if !body_ptr.is_null() {
            unsafe {
                (h.vtable.drop_body)(body_ptr);
                *h.body_ptr.get() = std::ptr::null_mut();
            }
        }
        h.state.store(STATE_CANCELLED, Ordering::Release);
        // Wake JoinHandle so it returns JoinError::Cancelled.
        // SAFETY: state=CANCELLED — exclusive join_waker access.
        let waker = unsafe { (*h.join_waker.get()).take() };
        if let Some(w) = waker {
            w.wake();
        }
        // Arc refcount decremented when `self` drops.
    }
}

// ── JoinHandle ────────────────────────────────────────────────────────────────

/// Future returned from `spawn()`. Resolves when the spawned task completes.
pub struct JoinHandle<T> {
    pub(crate) header: Arc<TaskHeader>,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Send + 'static> JoinHandle<T> {
    /// Request cancellation. If the task hasn't started or is idle, it will be
    /// dropped by the executor on its next scheduling pass.
    pub fn abort(&self) {
        // Try to flip IDLE → CANCELLED.
        let _ = self.header.state.compare_exchange(
            STATE_IDLE,
            STATE_CANCELLED,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        // Try to flip SCHEDULED → CANCELLED.
        let _ = self.header.state.compare_exchange(
            STATE_SCHEDULED,
            STATE_CANCELLED,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }
}

impl<T: Send + 'static> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let state = self.header.state.load(Ordering::Acquire);

        match state {
            STATE_COMPLETED => {
                // Take the output the task wrote into the header.
                // SAFETY: state=COMPLETED — the executor will not write output again.
                // Single-threaded: no concurrent reads from another JoinHandle.
                let boxed = unsafe { (*self.header.output.get()).take() };
                match boxed {
                    Some(any_val) => match any_val.downcast::<T>() {
                        Ok(val) => Poll::Ready(Ok(*val)),
                        Err(_) => Poll::Ready(Err(JoinError::Cancelled)), // type mismatch (bug)
                    },
                    None => Poll::Ready(Err(JoinError::Cancelled)), // already taken
                }
            }
            STATE_CANCELLED => Poll::Ready(Err(JoinError::Cancelled)),
            _ => {
                // Task still in flight — register our waker.
                // SAFETY: state is IDLE/SCHEDULED/RUNNING (not COMPLETED/CANCELLED).
                // The executor will write join_waker only after observing COMPLETED/CANCELLED,
                // which has not happened yet. Single-threaded: no concurrent poll.
                unsafe {
                    *self.header.join_waker.get() = Some(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn task_new_initial_state() {
        let (task, _jh) = Task::new(async { 42u32 });
        assert_eq!(task.header.state.load(Ordering::Acquire), STATE_SCHEDULED);
    }

    #[test]
    fn join_error_display() {
        assert_eq!(JoinError::Cancelled.to_string(), "task was cancelled");
        assert!(JoinError::Panic(Box::new("x"))
            .to_string()
            .contains("panicked"));
    }

    #[test]
    fn abort_from_idle_sets_cancelled() {
        let (task, jh) = Task::new(async { 1u32 });
        task.header.state.store(STATE_IDLE, Ordering::Release);
        jh.abort();
        assert_eq!(task.header.state.load(Ordering::Acquire), STATE_CANCELLED);
    }

    #[test]
    fn cancel_drops_future() {
        let dropped = Arc::new(AtomicBool::new(false));
        let d = dropped.clone();

        struct Bomb(Arc<AtomicBool>);
        impl Drop for Bomb {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        impl Future for Bomb {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
                Poll::Pending
            }
        }

        let (task, _jh) = Task::new(Bomb(d));
        task.cancel();
        assert!(
            dropped.load(Ordering::SeqCst),
            "future must be dropped on cancel"
        );
    }
}
