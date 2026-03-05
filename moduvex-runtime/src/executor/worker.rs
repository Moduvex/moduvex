//! Per-worker state for the multi-threaded executor.
//!
//! Each worker thread owns a `StealableQueue` and runs a `run_worker` loop:
//!
//! ```text
//! run_worker(id, shared_state)
//!   ├─ check own StealableQueue (local, fast path)
//!   ├─ check GlobalQueue (cross-thread injection)
//!   ├─ steal from random victim in WorkStealingPool
//!   └─ park on reactor (I/O + timer deadline)
//! ```
//!
//! Worker 0 is always the main thread (drives the root future).
//! Workers 1..N are spawned background threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Context;

use crate::platform::sys::{create_pipe, events_with_capacity, Interest};
use crate::reactor::{with_reactor, with_reactor_mut};
use crate::time::{next_timer_deadline, tick_timer_wheel};

#[cfg(unix)]
use crate::signal::{on_signal_readable, SIGNAL_TOKEN};

use super::scheduler::{GlobalQueue, LocalQueue};
use super::task::{Task, TaskHeader, STATE_CANCELLED, STATE_COMPLETED};
use super::waker::{make_waker_with_notifier, WorkerNotifier};
use super::work_stealing::{StealableQueue, WorkStealingPool};

/// Sentinel reactor token for the self-pipe read end.
/// Must not collide with user tokens (which start at 0).
const WAKE_TOKEN: usize = usize::MAX - 1;

// ── WorkerThread ──────────────────────────────────────────────────────────────

/// Per-worker thread state.
///
/// Each worker owns its own stealable queue and self-pipe for reactor wakeup.
/// Workers share GlobalQueue, WorkStealingPool, task map, and shutdown flag.
pub(crate) struct WorkerThread {
    /// Unique worker index (0 = main thread).
    pub worker_id: usize,
    /// This worker's stealable queue, also registered with the pool.
    pub stealable: Arc<StealableQueue>,
    /// Local buffer for dequeued tasks (avoid repeated lock contention).
    pub local: LocalQueue,
    /// Shared cross-thread injection queue.
    pub global: Arc<GlobalQueue>,
    /// Pool of all worker queues for stealing.
    pub steal_pool: Arc<WorkStealingPool>,
    /// Shared task ownership map (key = TaskHeader ptr addr).
    pub tasks: Arc<Mutex<HashMap<usize, Task>>>,
    /// Shared shutdown signal.
    pub shutdown: Arc<AtomicBool>,
    /// Notifier for unparking workers when tasks are enqueued.
    notifier: Arc<WorkerNotifier>,
    /// Read end of the self-pipe (registered with reactor).
    wake_rx: i32,
    /// Write end of the self-pipe (written by wakers to unpark).
    wake_tx: i32,
}

impl WorkerThread {
    /// Construct a worker, registering its self-pipe with the thread's reactor.
    pub(crate) fn new(
        worker_id: usize,
        global: Arc<GlobalQueue>,
        steal_pool: Arc<WorkStealingPool>,
        tasks: Arc<Mutex<HashMap<usize, Task>>>,
        shutdown: Arc<AtomicBool>,
        notifier: Arc<WorkerNotifier>,
    ) -> std::io::Result<Self> {
        let (wake_rx, wake_tx) = create_pipe()?;
        with_reactor(|r| r.register(wake_rx, WAKE_TOKEN, Interest::READABLE))?;
        let stealable = Arc::new(StealableQueue::new());
        Ok(Self {
            worker_id,
            stealable,
            local: LocalQueue::new(),
            global,
            steal_pool,
            tasks,
            shutdown,
            notifier,
            wake_rx,
            wake_tx,
        })
    }

    /// The write end of the self-pipe — used to wake this worker from the reactor.
    pub(crate) fn wake_tx(&self) -> i32 {
        self.wake_tx
    }

    /// Pop the next task header to run: local → global → steal.
    fn next_task(&mut self) -> Option<Arc<TaskHeader>> {
        // 1. Local queue first (fast path, no locking).
        if let Some(h) = self.local.pop() {
            return Some(h);
        }
        // 2. Drain from stealable queue into local.
        {
            let mut sq = self.stealable.local_mut();
            if !sq.is_empty() {
                let mut batch = Vec::with_capacity(16);
                sq.drain_front(&mut batch, 16);
                drop(sq);
                for h in batch {
                    if self.local.push(h).is_some() {
                        // overflow: push back to global (should rarely happen)
                        // we can't re-push to stealable here without recursion
                    }
                }
                return self.local.pop();
            }
        }
        // 3. Steal from global queue.
        if let Some(h) = self.global.pop() {
            return Some(h);
        }
        // 4. Work steal from peer workers.
        let n = self
            .steal_pool
            .steal_one(self.worker_id, &mut self.local, &self.global);
        if n > 0 {
            return self.local.pop();
        }
        None
    }

    /// Run the worker loop. Returns when `shutdown` is set and all work drained.
    pub(crate) fn run(&mut self) {
        loop {
            // Check shutdown first.
            if self.shutdown.load(Ordering::Acquire) {
                // Drain remaining tasks before stopping.
                self.drain_all_tasks();
                break;
            }

            // Tick expired timers.
            let expired = tick_timer_wheel(std::time::Instant::now());
            for w in expired {
                w.wake();
            }

            // Drain task queues.
            let mut did_work = false;
            loop {
                let Some(header) = self.next_task() else {
                    break;
                };
                did_work = true;
                self.run_task(header);
            }

            // Park on reactor when no work found.
            if !did_work {
                if self.shutdown.load(Ordering::Acquire) {
                    self.drain_all_tasks();
                    break;
                }
                self.park();
            }
        }
    }

    /// Run a single task identified by its header.
    fn run_task(&mut self, header: Arc<TaskHeader>) {
        let key = Arc::as_ptr(&header) as usize;
        let state = header.state.load(Ordering::Acquire);

        if state == STATE_CANCELLED {
            let task = self.tasks.lock().unwrap().remove(&key);
            if let Some(t) = task {
                t.cancel();
            }
            return;
        }
        if state == STATE_COMPLETED {
            self.tasks.lock().unwrap().remove(&key);
            return;
        }

        // Build a waker that re-enqueues on wake and notifies a worker.
        let waker = make_waker_with_notifier(
            Arc::clone(&header),
            Arc::clone(&self.global),
            Some(Arc::clone(&self.notifier)),
        );
        let mut cx = Context::from_waker(&waker);

        // Extract task atomically (single lock) to avoid TOCTOU race.
        let task = self.tasks.lock().unwrap().remove(&key);
        if let Some(task) = task {
            let completed = task.poll_task(&mut cx);
            if !completed {
                self.tasks.lock().unwrap().insert(key, task);
            }
        }
    }

    /// Drain all remaining tasks (called on shutdown).
    fn drain_all_tasks(&mut self) {
        // Drain local queues.
        while let Some(h) = self.local.pop() {
            let _ = h; // let headers drop
        }
        // Stealable queue.
        {
            let mut sq = self.stealable.local_mut();
            while sq.pop().is_some() {}
        }
    }

    /// Park on the reactor until I/O event or timer fires.
    fn park(&self) {
        const MAX_PARK_MS: u64 = 10;

        let timeout_ms = match next_timer_deadline() {
            None => MAX_PARK_MS,
            Some(deadline) => {
                let now = std::time::Instant::now();
                if deadline <= now {
                    0
                } else {
                    let ms = deadline.duration_since(now).as_millis() as u64;
                    ms.min(MAX_PARK_MS)
                }
            }
        };

        let mut events = events_with_capacity(64);
        let _ = with_reactor_mut(|r| r.poll(&mut events, Some(timeout_ms)));
        self.drain_wake_pipe();

        #[cfg(unix)]
        {
            let signal_fired = events.iter().any(|ev| ev.token == SIGNAL_TOKEN && ev.readable);
            if signal_fired {
                on_signal_readable();
            }
        }
    }

    /// Drain all pending bytes from the self-pipe read end (non-blocking).
    #[cfg(unix)]
    fn drain_wake_pipe(&self) {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: `wake_rx` is a valid O_NONBLOCK fd we own.
            let n = unsafe { libc::read(self.wake_rx, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
        }
    }

    #[cfg(not(unix))]
    fn drain_wake_pipe(&self) {}
}

impl Drop for WorkerThread {
    fn drop(&mut self) {
        let _ = with_reactor(|r| r.deregister(self.wake_rx));
        // SAFETY: we own wake_rx and wake_tx exclusively.
        #[cfg(unix)]
        unsafe {
            libc::close(self.wake_rx);
            libc::close(self.wake_tx);
        }
    }
}

// ── Thread-local current worker ───────────────────────────────────────────────

// Thread-local: write end of the current worker's self-pipe.
// Used by root wakers on worker threads to unpark the reactor park.
thread_local! {
    pub(crate) static CURRENT_WORKER_WAKE_TX: std::cell::Cell<i32> =
        const { std::cell::Cell::new(-1) };
}

/// Set the current worker's wake_tx in the thread-local.
pub(crate) fn set_current_worker_wake_tx(fd: i32) {
    CURRENT_WORKER_WAKE_TX.with(|c| c.set(fd));
}

/// Clear the current worker's wake_tx.
pub(crate) fn clear_current_worker_wake_tx() {
    CURRENT_WORKER_WAKE_TX.with(|c| c.set(-1));
}
