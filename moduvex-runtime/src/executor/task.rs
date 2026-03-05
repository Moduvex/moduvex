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
//!
//! # Thread Safety for Multi-Threaded Executor
//!
//! `join_waker` is now protected by a `Mutex` to allow safe concurrent access
//! between `JoinHandle::poll` (any worker thread) and `poll_task` / `cancel`
//! (any background worker). The double-check pattern in `JoinHandle::poll`
//! ensures the waker is never missed if a task completes concurrently.

use std::any::Any;
use std::cell::UnsafeCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Protected by a `Mutex` to allow safe concurrent access between
    /// `JoinHandle::poll` (on any worker thread) and `poll_task`/`cancel`
    /// (on any background worker). The double-check pattern in `JoinHandle::poll`
    /// ensures no missed wake-ups.
    pub join_waker: Mutex<Option<Waker>>,

    /// Raw pointer to the `Box<TaskBody<F>>` allocation.
    ///
    /// # Safety invariant
    /// Non-null from `Task::new` until `drop_body` is called by either
    /// `poll_task` (on completion) or `cancel`. Nulled immediately after.
    /// Only read/written while `state == STATE_RUNNING` or during cancellation.
    pub body_ptr: UnsafeCell<*mut ()>,

    /// Output value written by the vtable's `poll` on completion.
    ///
    /// Written with Release ordering on state → COMPLETED transition.
    /// Read with Acquire ordering after observing STATE_COMPLETED.
    /// The Release/Acquire pair on `state` provides the memory barrier.
    pub output: UnsafeCell<Option<Box<dyn Any + Send>>>,
}

// SAFETY: `body_ptr` and `output` are UnsafeCell fields accessed under the
// state machine's ordering guarantees:
// - `body_ptr`: only accessed while state == STATE_RUNNING (exclusive)
// - `output`: written before STATE_COMPLETED store (Release); read after
//   STATE_COMPLETED load (Acquire)
// `join_waker` is protected by its own Mutex.
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
            join_waker: Mutex::new(None),
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
            // Set COMPLETED with Release so the output write is visible to
            // any thread that observes STATE_COMPLETED with Acquire.
            h.state.store(STATE_COMPLETED, Ordering::Release);
            // Wake the JoinHandle waiter under the Mutex to prevent races
            // with JoinHandle::poll registering a waker concurrently.
            let waker = h.join_waker.lock().unwrap().take();
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
        // SAFETY: executor holds the Task exclusively; state = SCHEDULED or CANCELLED.
        let body_ptr = unsafe { *h.body_ptr.get() };
        if !body_ptr.is_null() {
            unsafe {
                (h.vtable.drop_body)(body_ptr);
                *h.body_ptr.get() = std::ptr::null_mut();
            }
        }
        h.state.store(STATE_CANCELLED, Ordering::Release);
        // Wake JoinHandle under the Mutex so no waker is missed.
        let waker = h.join_waker.lock().unwrap().take();
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
        // Fast path: check state before acquiring the waker lock.
        let state = self.header.state.load(Ordering::Acquire);

        if state == STATE_COMPLETED {
            return self.take_output();
        }
        if state == STATE_CANCELLED {
            return Poll::Ready(Err(JoinError::Cancelled));
        }

        // Task still in flight. Register waker under the Mutex to prevent a
        // race with poll_task completing the task simultaneously.
        //
        // Double-check pattern:
        //   1. Lock the waker Mutex.
        //   2. Re-read state (now synchronized with poll_task's Mutex lock).
        //   3. If still in-flight, store waker.
        //   4. If completed/cancelled, return Ready immediately.
        let mut guard = self.header.join_waker.lock().unwrap();
        // Re-check under lock: poll_task takes the lock before setting
        // STATE_COMPLETED, so if state is not COMPLETED here, we're safe to
        // store the waker and it will be taken by poll_task later.
        let state = self.header.state.load(Ordering::Acquire);
        match state {
            STATE_COMPLETED => {
                drop(guard);
                self.take_output()
            }
            STATE_CANCELLED => {
                drop(guard);
                Poll::Ready(Err(JoinError::Cancelled))
            }
            _ => {
                *guard = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl<T: Send + 'static> JoinHandle<T> {
    /// Take the output from the header after observing STATE_COMPLETED.
    fn take_output(self: Pin<&mut Self>) -> Poll<Result<T, JoinError>> {
        // SAFETY: state=COMPLETED (observed with Acquire). The worker that set
        // COMPLETED used Release ordering. The Release/Acquire pair establishes
        // happens-before: output write → COMPLETED store → our load → output read.
        let boxed = unsafe { (*self.header.output.get()).take() };
        match boxed {
            Some(any_val) => match any_val.downcast::<T>() {
                Ok(val) => Poll::Ready(Ok(*val)),
                Err(_) => Poll::Ready(Err(JoinError::Cancelled)),
            },
            None => Poll::Ready(Err(JoinError::Cancelled)), // already taken
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
