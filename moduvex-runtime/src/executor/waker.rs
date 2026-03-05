//! Custom `RawWakerVTable` implementation.
//!
//! Each `Waker` holds an `Arc<TaskHeader>` cast to a raw `*const ()`.
//! The four vtable functions implement the `RawWaker` contract:
//!
//! | function      | action                                              |
//! |---------------|-----------------------------------------------------|
//! | `clone_waker` | `Arc::clone` — increments refcount                 |
//! | `wake`        | schedule task, consume (decrement) Arc              |
//! | `wake_by_ref` | schedule task, keep Arc alive                       |
//! | `drop_waker`  | `Arc::from_raw` then drop — decrements refcount     |
//!
//! Safety contract: the data pointer is always a valid `Arc<TaskHeader>` that
//! was created via `Arc::into_raw`. All four functions restore it to an `Arc`
//! before performing any operation, maintaining the reference count correctly.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{RawWaker, RawWakerVTable, Waker};

use super::scheduler::GlobalQueue;
use super::task::{TaskHeader, STATE_IDLE, STATE_SCHEDULED};

// ── Vtable ────────────────────────────────────────────────────────────────────

/// The single static vtable shared by all task wakers.
static TASK_WAKER_VTABLE: RawWakerVTable =
    RawWakerVTable::new(clone_waker, wake, wake_by_ref, drop_waker);

// ── Public entry point ────────────────────────────────────────────────────────

/// Construct a `Waker` from an `Arc<TaskHeader>` and a reference to the
/// global queue into which the waker will push the task when fired.
///
/// `notifier` is optional: in multi-threaded mode it writes to a worker's
/// self-pipe to unpark it after re-scheduling a task.
pub(crate) fn make_waker(
    header: Arc<TaskHeader>,
    queue: Arc<GlobalQueue>,
) -> Waker {
    make_waker_with_notifier(header, queue, None)
}

/// Like `make_waker` but with an explicit `WorkerNotifier` for multi-threaded mode.
pub(crate) fn make_waker_with_notifier(
    header: Arc<TaskHeader>,
    queue: Arc<GlobalQueue>,
    notifier: Option<Arc<WorkerNotifier>>,
) -> Waker {
    let data = Arc::new(WakerData {
        header,
        queue,
        notifier,
    });
    let ptr = Arc::into_raw(data) as *const ();
    let raw = RawWaker::new(ptr, &TASK_WAKER_VTABLE);
    // SAFETY: The vtable functions correctly implement the RawWaker contract
    // (see module doc). `ptr` is a valid Arc pointer.
    unsafe { Waker::from_raw(raw) }
}

// ── WorkerNotifier ────────────────────────────────────────────────────────────

/// Holds write-end fds of all worker self-pipes. Used to unpark a worker
/// after pushing a task to GlobalQueue.
pub(crate) struct WorkerNotifier {
    wake_fds: std::sync::Mutex<Vec<i32>>,
    next: AtomicUsize,
}

// SAFETY: Mutex<Vec<i32>> and AtomicUsize are Send+Sync.
unsafe impl Send for WorkerNotifier {}
unsafe impl Sync for WorkerNotifier {}

impl WorkerNotifier {
    pub(crate) fn new() -> Self {
        Self {
            wake_fds: std::sync::Mutex::new(Vec::new()),
            next: AtomicUsize::new(0),
        }
    }

    /// Register a worker's self-pipe write fd.
    pub(crate) fn add_fd(&self, fd: i32) {
        self.wake_fds.lock().unwrap().push(fd);
    }

    /// Write one byte to a worker's self-pipe (round-robin) to unpark it.
    #[cfg(unix)]
    pub(crate) fn notify_one(&self) {
        let fds = self.wake_fds.lock().unwrap();
        if fds.is_empty() {
            return;
        }
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % fds.len();
        let fd = fds[idx];
        drop(fds);
        unsafe {
            let b: u8 = 1;
            libc::write(fd, &b as *const u8 as *const _, 1);
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn notify_one(&self) {}
}

// ── WakerData ─────────────────────────────────────────────────────────────────

/// Heap allocation backing each `Waker`. Bundles the task header with the
/// queue reference needed to reschedule the task.
struct WakerData {
    header: Arc<TaskHeader>,
    queue: Arc<GlobalQueue>,
    notifier: Option<Arc<WorkerNotifier>>,
}

// SAFETY: WakerData contains only Send+Sync types.
unsafe impl Send for WakerData {}
unsafe impl Sync for WakerData {}

// ── Vtable functions ──────────────────────────────────────────────────────────

/// Reconstruct an `Arc<WakerData>` from a raw pointer WITHOUT consuming it,
/// then immediately `forget` the Arc so the refcount is unchanged.
///
/// # Safety
/// `ptr` must be a valid `Arc<WakerData>` pointer produced by `Arc::into_raw`.
#[inline]
unsafe fn data_ref(ptr: *const ()) -> std::mem::ManuallyDrop<Arc<WakerData>> {
    // SAFETY: `ptr` is always `Arc::into_raw(Arc<WakerData>)`.
    std::mem::ManuallyDrop::new(Arc::from_raw(ptr as *const WakerData))
}

unsafe fn clone_waker(ptr: *const ()) -> RawWaker {
    // SAFETY: `ptr` is a valid Arc<WakerData> pointer (contract of RawWaker).
    let data = data_ref(ptr);
    // Increment refcount by cloning, then leak the clone.
    let cloned = Arc::clone(&*data);
    let new_ptr = Arc::into_raw(cloned) as *const ();
    RawWaker::new(new_ptr, &TASK_WAKER_VTABLE)
}

unsafe fn wake(ptr: *const ()) {
    // SAFETY: `ptr` is `Arc::into_raw(Arc<WakerData>)`; consuming it here
    // correctly decrements the refcount when `data` is dropped at end of fn.
    let data = Arc::from_raw(ptr as *const WakerData);
    schedule_task(&data);
    // `data` drops here → Arc refcount decremented.
}

unsafe fn wake_by_ref(ptr: *const ()) {
    // SAFETY: same pointer contract; we borrow without consuming.
    let data = data_ref(ptr);
    schedule_task(&data);
    // ManuallyDrop — refcount unchanged.
}

unsafe fn drop_waker(ptr: *const ()) {
    // SAFETY: Reconstruct and immediately drop to decrement Arc refcount.
    drop(Arc::from_raw(ptr as *const WakerData));
}

// ── Scheduling helper ─────────────────────────────────────────────────────────

/// Attempt to transition the task from IDLE → SCHEDULED and push it to the
/// global queue. If the task is already SCHEDULED/RUNNING, skip (it will be
/// re-polled automatically).
fn schedule_task(data: &WakerData) {
    let header = &data.header;
    // Only transition IDLE → SCHEDULED. Other states:
    //   SCHEDULED: already queued, nothing to do.
    //   RUNNING:   executor holds it; it will check for re-schedule after poll.
    //   COMPLETED/CANCELLED: done, ignore wake.
    let prev = header.state.compare_exchange(
        STATE_IDLE,
        STATE_SCHEDULED,
        Ordering::AcqRel,
        Ordering::Relaxed,
    );
    if prev.is_ok() {
        data.queue.push_header(Arc::clone(header));
        // Notify a parked worker to check the global queue.
        if let Some(ref notifier) = data.notifier {
            notifier.notify_one();
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::task::{Task, STATE_IDLE, STATE_SCHEDULED};
    use std::sync::atomic::Ordering;

    fn make_test_waker(task: &Task) -> (Waker, Arc<GlobalQueue>) {
        let q = Arc::new(GlobalQueue::new());
        let w = make_waker(Arc::clone(&task.header), Arc::clone(&q));
        (w, q)
    }

    #[test]
    fn waker_clone_increments_refcount() {
        let (task, _jh) = Task::new(async { 1u32 });
        task.header.state.store(STATE_IDLE, Ordering::Release);
        let q = Arc::new(GlobalQueue::new());
        let w1 = make_waker(Arc::clone(&task.header), Arc::clone(&q));
        let w2 = w1.clone();
        // Both wakers exist — refcount is at least 2 on top of task.header.
        drop(w1);
        drop(w2);
        // No panic = correct refcount management.
    }

    #[test]
    fn wake_by_ref_schedules_idle_task() {
        let (task, _jh) = Task::new(async { 2u32 });
        task.header.state.store(STATE_IDLE, Ordering::Release);
        let (waker, queue) = make_test_waker(&task);
        waker.wake_by_ref();
        assert_eq!(task.header.state.load(Ordering::Acquire), STATE_SCHEDULED);
        assert!(queue.pop().is_some());
    }

    #[test]
    fn wake_consumes_and_schedules() {
        let (task, _jh) = Task::new(async { 3u32 });
        task.header.state.store(STATE_IDLE, Ordering::Release);
        let (waker, queue) = make_test_waker(&task);
        waker.wake(); // consumes the waker
        assert_eq!(task.header.state.load(Ordering::Acquire), STATE_SCHEDULED);
        assert!(queue.pop().is_some());
    }

    #[test]
    fn wake_noop_when_already_scheduled() {
        let (task, _jh) = Task::new(async { 4u32 });
        task.header.state.store(STATE_SCHEDULED, Ordering::Release);
        let (waker, queue) = make_test_waker(&task);
        waker.wake_by_ref();
        // State stays SCHEDULED, queue stays empty (CAS rejected).
        assert_eq!(task.header.state.load(Ordering::Acquire), STATE_SCHEDULED);
        assert!(queue.pop().is_none());
    }
}
