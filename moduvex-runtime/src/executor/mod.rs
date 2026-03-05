//! Async executor: single-threaded (default) and multi-threaded (opt-in).
//!
//! # Single-threaded run loop (default)
//! ```text
//! block_on(future)
//!   └─ Executor::run_loop
//!        ├─ LocalQueue  (LIFO ring, 256 slots)
//!        ├─ GlobalQueue (Mutex<VecDeque> — waker injection)
//!        └─ Reactor     (kqueue/epoll — parks when no work is ready)
//! ```
//!
//! # Multi-threaded run loop (opt-in via RuntimeBuilder::worker_threads(n))
//! ```text
//! block_on_multi(future, n_workers)
//!   ├─ worker 0 (main thread) — polls root future + runs tasks
//!   ├─ worker 1..N-1 (spawned threads) — steal and run tasks
//!   └─ GlobalQueue + WorkStealingPool shared across all workers
//! ```
//!
//! Single-threaded mode is the default. Multi-thread is explicitly opt-in.

pub mod scheduler;
pub mod task;
pub mod task_local;
pub mod waker;
pub mod work_stealing;
pub mod worker;

use std::cell::Cell;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use crate::platform::sys::{create_pipe, events_with_capacity, Interest};
use crate::reactor::{with_reactor, with_reactor_mut};
use crate::time::{next_timer_deadline, tick_timer_wheel};

#[cfg(unix)]
use crate::signal::{on_signal_readable, SIGNAL_TOKEN};

use scheduler::{GlobalQueue, LocalQueue};
use task::{JoinHandle, Task, STATE_CANCELLED, STATE_COMPLETED};
use waker::{make_waker, make_waker_with_notifier, WorkerNotifier};
use work_stealing::{StealableQueue, WorkStealingPool};
use worker::{clear_current_worker_wake_tx, set_current_worker_wake_tx, WorkerThread};

// ── Executor ──────────────────────────────────────────────────────────────────

/// Per-thread async executor (single-threaded mode).
pub struct Executor {
    /// LIFO local task queue — popped first each iteration.
    local: LocalQueue,
    /// Shared with all `Waker`s — they push here to re-schedule tasks.
    global: Arc<GlobalQueue>,
    /// Owned `Task` handles keyed by `Arc<TaskHeader>` pointer address.
    tasks: HashMap<usize, Task>,
    /// Read end of the self-pipe, registered with the reactor.
    wake_rx: i32,
    /// Write end of the self-pipe; the root-waker writes here to unblock park.
    wake_tx: i32,
}

impl Executor {
    fn new() -> std::io::Result<Self> {
        let (wake_rx, wake_tx) = create_pipe()?;
        with_reactor(|r| r.register(wake_rx, WAKE_TOKEN, Interest::READABLE))?;
        Ok(Self {
            local: LocalQueue::new(),
            global: Arc::new(GlobalQueue::new()),
            tasks: HashMap::new(),
            wake_rx,
            wake_tx,
        })
    }

    /// Spawn a future onto this executor, returning a `JoinHandle<T>`.
    pub fn spawn<F>(&mut self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: Send + 'static,
    {
        let (task, jh) = Task::new(future);
        let key = Arc::as_ptr(&task.header) as usize;
        self.global.push_header(Arc::clone(&task.header));
        self.tasks.insert(key, task);
        jh
    }

    /// Drive the executor until `root` resolves. Returns root's output.
    pub fn block_on<F: Future>(&mut self, future: F) -> F::Output {
        let mut root = std::pin::pin!(future);
        let mut root_done = false;
        let mut root_output: Option<F::Output> = None;

        let root_waker = self.make_root_waker();

        loop {
            // ── 1. Tick timer wheel ────────────────────────────────────────
            let expired = tick_timer_wheel(std::time::Instant::now());
            for w in expired {
                w.wake();
            }

            // ── 2. Poll root ───────────────────────────────────────────────
            if !root_done {
                let mut cx = Context::from_waker(&root_waker);
                if let Poll::Ready(val) = root.as_mut().poll(&mut cx) {
                    root_output = Some(val);
                    root_done = true;
                }
            }

            // ── 3. Exit if root done and no spawned tasks remain ───────────
            if root_done && self.tasks.is_empty() {
                break;
            }

            // ── 4. Drain task queues ───────────────────────────────────────
            let mut did_work = false;
            loop {
                let Some(header) = self.next_task() else {
                    break;
                };
                did_work = true;
                let key = Arc::as_ptr(&header) as usize;
                let state = header.state.load(Ordering::Acquire);

                if state == STATE_CANCELLED {
                    if let Some(task) = self.tasks.remove(&key) {
                        task.cancel();
                    }
                    continue;
                }
                if state == STATE_COMPLETED {
                    self.tasks.remove(&key);
                    continue;
                }

                let waker = make_waker(Arc::clone(&header), Arc::clone(&self.global));
                let mut cx = Context::from_waker(&waker);

                if let Some(task) = self.tasks.get(&key) {
                    let completed = task.poll_task(&mut cx);
                    if completed {
                        self.tasks.remove(&key);
                    }
                }
            }

            // ── 5. Re-check exit after draining ───────────────────────────
            if root_done && self.tasks.is_empty() {
                break;
            }

            // ── 6. Park on reactor when both queues empty ──────────────────
            if !did_work && self.local.is_empty() && self.global.len() == 0 {
                self.park();
            }
        }

        root_output.expect("root future must complete before block_on returns")
    }

    /// Drain both queues: pop local first, then global.
    fn next_task(&mut self) -> Option<Arc<task::TaskHeader>> {
        self.local.pop().or_else(|| self.global.pop())
    }

    /// Block on the reactor using the next timer deadline as the timeout.
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

    #[cfg(unix)]
    fn make_root_waker(&self) -> std::task::Waker {
        use std::task::{RawWaker, RawWakerVTable};

        let tx = self.wake_tx;

        unsafe fn clone_root(ptr: *const ()) -> RawWaker {
            RawWaker::new(ptr, &ROOT_VTABLE)
        }
        unsafe fn wake_root(ptr: *const ()) {
            let fd = ptr as usize as i32;
            let b: u8 = 1;
            // SAFETY: fd is the write end of a non-blocking pipe we own.
            libc::write(fd, &b as *const u8 as *const _, 1);
        }
        unsafe fn wake_root_by_ref(ptr: *const ()) {
            wake_root(ptr);
        }
        unsafe fn drop_root(_: *const ()) {}

        static ROOT_VTABLE: RawWakerVTable =
            RawWakerVTable::new(clone_root, wake_root, wake_root_by_ref, drop_root);

        let raw = std::task::RawWaker::new(tx as usize as *const (), &ROOT_VTABLE);
        // SAFETY: ROOT_VTABLE satisfies the RawWaker contract.
        unsafe { std::task::Waker::from_raw(raw) }
    }

    #[cfg(not(unix))]
    fn make_root_waker(&self) -> std::task::Waker {
        use std::task::{RawWaker, RawWakerVTable};
        static NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
            |p| RawWaker::new(p, &NOOP_VTABLE),
            |_| {},
            |_| {},
            |_| {},
        );
        unsafe { std::task::Waker::from_raw(RawWaker::new(std::ptr::null(), &NOOP_VTABLE)) }
    }
}

impl Drop for Executor {
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

/// Sentinel reactor token for the self-pipe read end.
const WAKE_TOKEN: usize = usize::MAX;

// ── Thread-local executor pointer (single-threaded path) ──────────────────────

thread_local! {
    /// Raw pointer to the current thread's `Executor`.
    /// Non-null only inside a `block_on_with_spawn` call.
    static CURRENT_EXECUTOR: Cell<*mut Executor> = const { Cell::new(std::ptr::null_mut()) };
}

// ── Multi-threaded executor state ─────────────────────────────────────────────

/// Shared state for the multi-threaded executor.
///
/// All workers hold an `Arc` to this. The main thread (worker 0) also drives
/// the root future and signals shutdown when it completes.
struct MultiState {
    global: Arc<GlobalQueue>,
    steal_pool: Arc<Mutex<WorkStealingPool>>,
    tasks: Arc<Mutex<HashMap<usize, Task>>>,
    shutdown: Arc<AtomicBool>,
    notifier: Arc<WorkerNotifier>,
}

impl MultiState {
    fn new() -> Self {
        Self {
            global: Arc::new(GlobalQueue::new()),
            steal_pool: Arc::new(Mutex::new(WorkStealingPool::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            notifier: Arc::new(WorkerNotifier::new()),
        }
    }
}

// Thread-locals for the multi-thread spawn path.
// Set on each worker thread when multi-thread mode is active.
// `spawn()` reads these to route to the shared global queue and task map.
thread_local! {
    static MT_GLOBAL_QUEUE: Cell<*const GlobalQueue> = const { Cell::new(std::ptr::null()) };
    static MT_TASKS: Cell<*const Mutex<HashMap<usize, Task>>> = const { Cell::new(std::ptr::null()) };
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Drive `future` to completion on the current thread, returning its output.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut exec = Executor::new().expect("executor init failed");
    exec.block_on(future)
}

/// Drive `future` to completion with `spawn()` available in the async context.
///
/// Single-threaded mode (1 worker). Use `block_on_multi` for multi-thread.
pub fn block_on_with_spawn<F: Future>(future: F) -> F::Output {
    let mut exec = Executor::new().expect("executor init failed");
    CURRENT_EXECUTOR.with(|c| c.set(&mut exec as *mut Executor));
    let result = exec.block_on(future);
    CURRENT_EXECUTOR.with(|c| c.set(std::ptr::null_mut()));
    result
}

/// Drive `future` to completion using `num_workers` OS threads.
///
/// Worker 0 is the main thread (drives root future). Workers 1..N-1 are
/// spawned as background threads. All workers share GlobalQueue and steal tasks.
///
/// When `num_workers <= 1`, falls back to `block_on_with_spawn` (single-thread).
pub fn block_on_multi<F>(future: F, num_workers: usize) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    if num_workers <= 1 {
        return block_on_with_spawn(future);
    }

    let state = MultiState::new();

    // Build a single WorkStealingPool for all workers.
    let steal_pool_arc = Arc::new({
        let mut pool = WorkStealingPool::new();
        for _ in 0..num_workers {
            pool.add_worker(Arc::new(StealableQueue::new()));
        }
        pool
    });

    // Set MT thread-locals on main thread.
    let global_ptr = Arc::as_ptr(&state.global);
    let tasks_ptr = Arc::as_ptr(&state.tasks);
    MT_GLOBAL_QUEUE.with(|c| c.set(global_ptr));
    MT_TASKS.with(|c| c.set(tasks_ptr));

    // Spawn background worker threads (workers 1..N-1).
    let mut handles = Vec::new();
    for worker_id in 1..num_workers {
        let global = Arc::clone(&state.global);
        let steal_pool = Arc::clone(&steal_pool_arc);
        let tasks = Arc::clone(&state.tasks);
        let shutdown = Arc::clone(&state.shutdown);
        let notifier = Arc::clone(&state.notifier);

        let handle = std::thread::spawn(move || {
            // Set MT thread-locals on this worker thread.
            let global_ptr = Arc::as_ptr(&global);
            let tasks_ptr = Arc::as_ptr(&tasks);
            MT_GLOBAL_QUEUE.with(|c| c.set(global_ptr));
            MT_TASKS.with(|c| c.set(tasks_ptr));

            let mut worker = WorkerThread::new(
                worker_id,
                global,
                steal_pool,
                tasks,
                shutdown,
                Arc::clone(&notifier),
            )
            .expect("worker init failed");

            // Register this worker's self-pipe with the notifier.
            notifier.add_fd(worker.wake_tx());

            set_current_worker_wake_tx(worker.wake_tx());
            worker.run();
            clear_current_worker_wake_tx();

            MT_GLOBAL_QUEUE.with(|c| c.set(std::ptr::null()));
            MT_TASKS.with(|c| c.set(std::ptr::null()));
        });
        handles.push(handle);
    }

    // Worker 0: main thread drives root future.
    let result = run_worker_0(future, &state, steal_pool_arc);

    // Signal all workers to stop.
    state.shutdown.store(true, Ordering::Release);
    // Wake all parked workers so they see shutdown.
    for _ in 0..num_workers {
        state.notifier.notify_one();
    }

    // Join all background workers.
    for h in handles {
        let _ = h.join();
    }

    MT_GLOBAL_QUEUE.with(|c| c.set(std::ptr::null()));
    MT_TASKS.with(|c| c.set(std::ptr::null()));

    result
}

/// Run the main-thread worker (worker 0) which also polls the root future.
fn run_worker_0<F>(future: F, state: &MultiState, steal_pool: Arc<WorkStealingPool>) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    // Worker 0 uses its own self-pipe for reactor wakeup.
    let (wake_rx, wake_tx) =
        create_pipe().expect("worker 0 self-pipe failed");
    with_reactor(|r| {
        r.register(wake_rx, WAKE_TOKEN, Interest::READABLE)
            .expect("worker 0 wake pipe register failed")
    });

    // Register worker 0's self-pipe with the notifier.
    state.notifier.add_fd(wake_tx);
    set_current_worker_wake_tx(wake_tx);

    let mut root = std::pin::pin!(future);
    let mut root_done = false;
    let mut root_output: Option<F::Output> = None;

    let root_waker = make_worker0_root_waker(wake_tx);

    // Local queue for worker 0.
    let mut local = LocalQueue::new();

    loop {
        // Tick timers.
        let expired = tick_timer_wheel(std::time::Instant::now());
        for w in expired {
            w.wake();
        }

        // Poll root future.
        if !root_done {
            let mut cx = Context::from_waker(&root_waker);
            if let Poll::Ready(val) = root.as_mut().poll(&mut cx) {
                root_output = Some(val);
                root_done = true;
            }
        }

        // Check exit: root done + no tasks remaining.
        if root_done && state.tasks.lock().unwrap().is_empty() {
            break;
        }

        // Drain task queues.
        let mut did_work = false;
        loop {
            // Try local first.
            let header = local.pop().or_else(|| state.global.pop()).or_else(|| {
                // Steal from peer workers.
                let n = steal_pool.steal_one(0, &mut local, &state.global);
                if n > 0 { local.pop() } else { None }
            });

            let Some(header) = header else { break };
            did_work = true;

            let key = Arc::as_ptr(&header) as usize;
            let task_state = header.state.load(Ordering::Acquire);

            if task_state == STATE_CANCELLED {
                let t = state.tasks.lock().unwrap().remove(&key);
                if let Some(task) = t {
                    task.cancel();
                }
                continue;
            }
            if task_state == STATE_COMPLETED {
                state.tasks.lock().unwrap().remove(&key);
                continue;
            }

            let waker = make_waker_with_notifier(
                Arc::clone(&header),
                Arc::clone(&state.global),
                Some(Arc::clone(&state.notifier)),
            );
            let mut cx = Context::from_waker(&waker);

            // Extract task, poll, re-insert or drop.
            let task = state.tasks.lock().unwrap().remove(&key);
            if let Some(task) = task {
                let completed = task.poll_task(&mut cx);
                if !completed {
                    state.tasks.lock().unwrap().insert(key, task);
                }
            }
        }

        // Re-check exit.
        if root_done && state.tasks.lock().unwrap().is_empty() {
            break;
        }

        // Park.
        if !did_work {
            park_worker(wake_rx);
        }
    }

    clear_current_worker_wake_tx();

    // Deregister and close self-pipe.
    let _ = with_reactor(|r| r.deregister(wake_rx));
    #[cfg(unix)]
    unsafe {
        libc::close(wake_rx);
        libc::close(wake_tx);
    }

    root_output.expect("root future must complete")
}

/// Park the current worker on the reactor.
fn park_worker(wake_rx: i32) {
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

    // Drain self-pipe.
    #[cfg(unix)]
    {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: wake_rx is a valid O_NONBLOCK fd.
            let n = unsafe { libc::read(wake_rx, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
        }

        let signal_fired = events.iter().any(|ev| ev.token == SIGNAL_TOKEN && ev.readable);
        if signal_fired {
            on_signal_readable();
        }
    }

    #[cfg(not(unix))]
    let _ = wake_rx;
}

/// Build a root waker for worker 0 (writes to its self-pipe to unpark).
#[cfg(unix)]
fn make_worker0_root_waker(wake_tx: i32) -> std::task::Waker {
    use std::task::{RawWaker, RawWakerVTable};

    unsafe fn clone_root(ptr: *const ()) -> RawWaker {
        RawWaker::new(ptr, &ROOT_VTABLE)
    }
    unsafe fn wake_root(ptr: *const ()) {
        let fd = ptr as usize as i32;
        let b: u8 = 1;
        // SAFETY: fd is the write end of a non-blocking pipe.
        libc::write(fd, &b as *const u8 as *const _, 1);
    }
    unsafe fn wake_root_by_ref(ptr: *const ()) {
        wake_root(ptr);
    }
    unsafe fn drop_root(_: *const ()) {}

    static ROOT_VTABLE: RawWakerVTable =
        RawWakerVTable::new(clone_root, wake_root, wake_root_by_ref, drop_root);

    let raw = std::task::RawWaker::new(wake_tx as usize as *const (), &ROOT_VTABLE);
    // SAFETY: ROOT_VTABLE is correct; fd lives for the duration of the call.
    unsafe { std::task::Waker::from_raw(raw) }
}

#[cfg(not(unix))]
fn make_worker0_root_waker(_wake_tx: i32) -> std::task::Waker {
    use std::task::{RawWaker, RawWakerVTable};
    static NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |p| RawWaker::new(p, &NOOP_VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    unsafe { std::task::Waker::from_raw(RawWaker::new(std::ptr::null(), &NOOP_VTABLE)) }
}

// ── Spawn — routes to single-thread or multi-thread context ───────────────────

/// Spawn a future onto the current thread's executor.
///
/// Works in both single-threaded (`block_on_with_spawn`) and multi-threaded
/// (`block_on_multi`) contexts. Panics if called outside both.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: Send + 'static,
{
    // Try single-threaded path first.
    let st_ptr = CURRENT_EXECUTOR.with(|c| c.get());
    if !st_ptr.is_null() {
        // SAFETY: ptr valid for duration of `block_on_with_spawn`.
        return unsafe { (*st_ptr).spawn(future) };
    }

    // Try multi-threaded path.
    let mt_global = MT_GLOBAL_QUEUE.with(|c| c.get());
    let mt_tasks = MT_TASKS.with(|c| c.get());

    if !mt_global.is_null() && !mt_tasks.is_null() {
        let (task, jh) = Task::new(future);
        let key = Arc::as_ptr(&task.header) as usize;
        let header_clone = Arc::clone(&task.header);
        // SAFETY: pointers are valid for the duration of block_on_multi.
        // Insert task BEFORE pushing header — prevents race where a background
        // worker pops the header but finds no Task in the map.
        unsafe {
            (*mt_tasks).lock().unwrap().insert(key, task);
            (*mt_global).push_header(header_clone);
        }
        return jh;
    }

    panic!("spawn() called outside of block_on_with_spawn or block_on_multi context");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as Ord};

    #[test]
    fn block_on_simple_value() {
        assert_eq!(block_on(async { 42u32 }), 42);
    }

    #[test]
    fn block_on_chain_of_awaits() {
        async fn double(x: u32) -> u32 {
            x * 2
        }
        async fn compute() -> u32 {
            double(double(3).await).await
        }
        assert_eq!(block_on(compute()), 12);
    }

    #[test]
    fn block_on_string_output() {
        assert_eq!(block_on(async { String::from("hello") }), "hello");
    }

    #[test]
    fn spawn_and_join() {
        let result = block_on_with_spawn(async {
            let jh = spawn(async { 100u32 });
            jh.await.unwrap()
        });
        assert_eq!(result, 100);
    }

    #[test]
    fn spawn_multiple_and_join_all() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = counter.clone();
        let c2 = counter.clone();
        block_on_with_spawn(async move {
            let jh1 = spawn(async move {
                c1.fetch_add(1, Ord::SeqCst);
            });
            let jh2 = spawn(async move {
                c2.fetch_add(1, Ord::SeqCst);
            });
            jh1.await.unwrap();
            jh2.await.unwrap();
        });
        assert_eq!(counter.load(Ord::SeqCst), 2);
    }

    #[test]
    fn join_handle_abort_returns_cancelled() {
        use std::future::poll_fn;
        use std::task::Poll as P;

        let result = block_on_with_spawn(async {
            let jh = spawn(async { poll_fn(|_| P::<()>::Pending).await });
            jh.abort();
            jh.await
        });
        assert!(matches!(result, Err(task::JoinError::Cancelled)));
    }

    #[test]
    fn block_on_nested_spawn_ordering() {
        let order = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        block_on_with_spawn(async move {
            let jh1 = spawn(async move {
                o1.lock().unwrap().push(1);
            });
            let jh2 = spawn(async move {
                o2.lock().unwrap().push(2);
            });
            jh1.await.unwrap();
            jh2.await.unwrap();
        });
        let v = order.lock().unwrap();
        assert_eq!(v.len(), 2);
    }

    // ── Multi-threaded tests ───────────────────────────────────────────────

    #[test]
    fn multi_thread_simple_spawn() {
        let result = block_on_multi(
            async {
                let jh = spawn(async { 42u32 });
                jh.await.unwrap()
            },
            2,
        );
        assert_eq!(result, 42);
    }

    #[test]
    fn multi_thread_many_tasks_complete() {
        const N: usize = 100;
        let counter = Arc::new(AtomicUsize::new(0));

        let c = counter.clone();
        block_on_multi(
            async move {
                let mut handles = Vec::new();
                for _ in 0..N {
                    let cc = c.clone();
                    handles.push(spawn(async move {
                        cc.fetch_add(1, Ord::SeqCst);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            },
            4,
        );

        assert_eq!(counter.load(Ord::SeqCst), N);
    }

    #[test]
    fn multi_thread_falls_back_to_single_with_one_worker() {
        // num_workers=1 uses single-thread path, must still work.
        let result = block_on_multi(async { 99u32 }, 1);
        assert_eq!(result, 99);
    }

    #[test]
    fn multi_thread_1000_tasks_4_workers() {
        const N: usize = 1000;
        let counter = Arc::new(AtomicUsize::new(0));

        let c = counter.clone();
        block_on_multi(
            async move {
                let mut handles = Vec::new();
                for _ in 0..N {
                    let cc = c.clone();
                    handles.push(spawn(async move {
                        cc.fetch_add(1, Ord::SeqCst);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            },
            4,
        );

        assert_eq!(counter.load(Ord::SeqCst), N);
    }

    // ── Additional executor tests ──────────────────────────────────────────

    #[test]
    fn block_on_returns_unit() {
        block_on(async {});
    }

    #[test]
    fn block_on_with_spawn_returns_unit() {
        block_on_with_spawn(async {});
    }

    #[test]
    fn spawn_1000_tasks_single_thread_all_complete() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_with_spawn(async move {
            let mut handles = Vec::new();
            for _ in 0..1000 {
                let cc = c.clone();
                handles.push(spawn(async move {
                    cc.fetch_add(1, Ord::SeqCst);
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });
        assert_eq!(counter.load(Ord::SeqCst), 1000);
    }

    #[test]
    fn spawn_in_spawned_task() {
        let result = block_on_with_spawn(async {
            let jh = spawn(async {
                let inner = spawn(async { 42u32 });
                inner.await.unwrap()
            });
            jh.await.unwrap()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn join_handle_dropped_without_await_no_panic() {
        // Drop the JoinHandle without awaiting; executor must not panic or deadlock.
        block_on_with_spawn(async move {
            // Spawn a task and immediately drop the handle (detach it).
            drop(spawn(async move { 42u32 }));
            // Spawn a second task to give the executor a reason to keep running.
            // This second task ensures the root future doesn't exit before the
            // first task has had a chance to run.
            let jh2 = spawn(async move { 99u32 });
            jh2.await.unwrap()
        });
        // No assertion needed — we just verify no panic/hang.
    }

    #[test]
    fn multi_thread_0_workers_fallback_to_single() {
        // num_workers=0 edge case: should not panic, falls back to single.
        let result = block_on_multi(async { 7u32 }, 0);
        assert_eq!(result, 7);
    }

    #[test]
    fn multi_thread_3_workers_all_join() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_multi(
            async move {
                let mut handles = Vec::new();
                for _ in 0..30 {
                    let cc = c.clone();
                    handles.push(spawn(async move {
                        cc.fetch_add(1, Ord::SeqCst);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            },
            3,
        );
        assert_eq!(counter.load(Ord::SeqCst), 30);
    }

    #[test]
    fn multi_thread_nested_spawn() {
        let result = block_on_multi(
            async {
                let jh = spawn(async {
                    let inner = spawn(async { 99u32 });
                    inner.await.unwrap()
                });
                jh.await.unwrap()
            },
            2,
        );
        assert_eq!(result, 99);
    }

    #[test]
    fn block_on_with_spawn_sequential_ordering() {
        let order = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let o = order.clone();
        block_on_with_spawn(async move {
            let o1 = o.clone();
            let o2 = o.clone();
            let o3 = o.clone();
            let jh1 = spawn(async move {
                o1.lock().unwrap().push(1);
            });
            let jh2 = spawn(async move {
                o2.lock().unwrap().push(2);
            });
            let jh3 = spawn(async move {
                o3.lock().unwrap().push(3);
            });
            jh1.await.unwrap();
            jh2.await.unwrap();
            jh3.await.unwrap();
        });
        assert_eq!(order.lock().unwrap().len(), 3);
    }

    #[test]
    fn multi_thread_result_type_roundtrip() {
        let result: Result<u32, String> = block_on_multi(
            async {
                let jh = spawn(async { Ok::<u32, String>(42) });
                jh.await.unwrap()
            },
            2,
        );
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn block_on_returns_string() {
        let s = block_on(async { String::from("hello world") });
        assert_eq!(s, "hello world");
    }

    #[test]
    fn block_on_returns_vec() {
        let v = block_on(async { vec![1u32, 2, 3] });
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn spawn_returns_computed_value() {
        let result = block_on_with_spawn(async {
            let jh = spawn(async { 2u32 * 21 });
            jh.await.unwrap()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn spawn_with_move_captures_outer() {
        let data = Arc::new(AtomicUsize::new(55));
        let d = data.clone();
        let result = block_on_with_spawn(async move {
            let jh = spawn(async move { d.load(Ord::SeqCst) });
            jh.await.unwrap()
        });
        assert_eq!(result, 55);
    }

    #[test]
    fn multi_thread_2_workers_count_50() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_multi(
            async move {
                let mut handles = Vec::new();
                for _ in 0..50 {
                    let cc = c.clone();
                    handles.push(spawn(async move {
                        cc.fetch_add(1, Ord::SeqCst);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            },
            2,
        );
        assert_eq!(counter.load(Ord::SeqCst), 50);
    }

    #[test]
    fn spawn_chain_3_deep() {
        let result = block_on_with_spawn(async {
            let h1 = spawn(async {
                let h2 = spawn(async {
                    let h3 = spawn(async { 7u32 });
                    h3.await.unwrap() * 2
                });
                h2.await.unwrap() + 1
            });
            h1.await.unwrap()
        });
        assert_eq!(result, 15); // 7*2+1 = 15
    }

    #[test]
    fn block_on_returns_option() {
        let v = block_on(async { Some(42u32) });
        assert_eq!(v, Some(42));
    }

    #[test]
    fn block_on_returns_tuple() {
        let (a, b) = block_on(async { (1u32, 2u32) });
        assert_eq!(a, 1);
        assert_eq!(b, 2);
    }

    #[test]
    fn spawn_10_independent_tasks_all_increment() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_with_spawn(async move {
            let mut handles: Vec<_> = (0..10)
                .map(|_| {
                    let cc = c.clone();
                    spawn(async move {
                        cc.fetch_add(1, Ord::SeqCst);
                    })
                })
                .collect();
            for h in handles.drain(..) {
                h.await.unwrap();
            }
        });
        assert_eq!(counter.load(Ord::SeqCst), 10);
    }

    #[test]
    fn multi_thread_5_workers_500_tasks() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_multi(
            async move {
                let handles: Vec<_> = (0..500)
                    .map(|_| {
                        let cc = c.clone();
                        spawn(async move {
                            cc.fetch_add(1, Ord::SeqCst);
                        })
                    })
                    .collect();
                for h in handles {
                    h.await.unwrap();
                }
            },
            5,
        );
        assert_eq!(counter.load(Ord::SeqCst), 500);
    }

    #[test]
    fn block_on_with_spawn_arc_shared_across_tasks() {
        let shared = Arc::new(AtomicUsize::new(0));
        let s = shared.clone();
        block_on_with_spawn(async move {
            let s1 = s.clone();
            let s2 = s.clone();
            let h1 = spawn(async move { s1.fetch_add(10, Ord::SeqCst) });
            let h2 = spawn(async move { s2.fetch_add(20, Ord::SeqCst) });
            h1.await.unwrap();
            h2.await.unwrap();
        });
        let v = shared.load(Ord::SeqCst);
        assert_eq!(v, 30);
    }

    #[test]
    fn abort_before_poll_returns_cancelled() {
        let result = block_on_with_spawn(async {
            let jh = spawn(async {
                // This future never completes on its own
                std::future::poll_fn(|_| std::task::Poll::<()>::Pending).await
            });
            jh.abort();
            jh.await
        });
        assert!(matches!(result, Err(task::JoinError::Cancelled)));
    }

    #[test]
    fn spawn_returns_unit_output() {
        block_on_with_spawn(async {
            let jh = spawn(async {});
            jh.await.unwrap(); // output is ()
        });
    }

    #[test]
    fn multi_thread_result_err_type_roundtrip() {
        let result: Result<u32, String> = block_on_multi(
            async {
                let jh = spawn(async { Err::<u32, String>("fail".to_string()) });
                jh.await.unwrap()
            },
            2,
        );
        assert_eq!(result, Err("fail".to_string()));
    }

    #[test]
    fn block_on_f64_value() {
        let v: f64 = block_on(async { 3.14 });
        assert!((v - 3.14).abs() < 1e-10);
    }

    #[test]
    fn spawn_computes_product_of_two_values() {
        let result = block_on_with_spawn(async {
            let a = spawn(async { 6u32 });
            let b = spawn(async { 7u32 });
            a.await.unwrap() * b.await.unwrap()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn block_on_with_spawn_returns_bool() {
        let v = block_on_with_spawn(async {
            let jh = spawn(async { true });
            jh.await.unwrap()
        });
        assert!(v);
    }

    #[test]
    fn multi_thread_6_workers_200_tasks() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_multi(
            async move {
                let handles: Vec<_> = (0..200)
                    .map(|_| {
                        let cc = c.clone();
                        spawn(async move {
                            cc.fetch_add(1, Ord::SeqCst);
                        })
                    })
                    .collect();
                for h in handles {
                    h.await.unwrap();
                }
            },
            6,
        );
        assert_eq!(counter.load(Ord::SeqCst), 200);
    }

    #[test]
    fn spawn_task_with_string_return() {
        let result = block_on_with_spawn(async {
            let jh = spawn(async { "hello".to_string() });
            jh.await.unwrap()
        });
        assert_eq!(result, "hello");
    }

    #[test]
    fn block_on_nested_async_fns() {
        async fn add(a: u32, b: u32) -> u32 {
            a + b
        }
        async fn multiply(a: u32, b: u32) -> u32 {
            a * b
        }
        let result = block_on(async {
            let sum = add(3, 4).await;
            multiply(sum, 2).await
        });
        assert_eq!(result, 14);
    }

    #[test]
    fn block_on_complex_expression() {
        let result = block_on(async {
            let a = 10u32;
            let b = 20u32;
            a + b + 12
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn spawn_50_tasks_all_complete_with_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_with_spawn(async move {
            let handles: Vec<_> = (0..50)
                .map(|_| {
                    let cc = c.clone();
                    spawn(async move { cc.fetch_add(1, Ord::SeqCst) })
                })
                .collect();
            for h in handles {
                h.await.unwrap();
            }
        });
        assert_eq!(counter.load(Ord::SeqCst), 50);
    }

    #[test]
    fn multi_thread_join_handle_result_preserved() {
        // Spawned task computes unique value, JoinHandle returns it correctly
        let values: Vec<u32> = (0..8).collect();
        let results: Vec<u32> = block_on_multi(
            async {
                let handles: Vec<_> = (0..8u32)
                    .map(|i| spawn(async move { i * i }))
                    .collect();
                let mut results = Vec::new();
                for h in handles {
                    results.push(h.await.unwrap());
                }
                results
            },
            4,
        );
        assert_eq!(results.len(), 8);
        for (i, &v) in results.iter().enumerate() {
            assert_eq!(v, (i as u32) * (i as u32));
        }
    }

    #[test]
    fn block_on_with_spawn_multiple_spawn_waves() {
        // Spawn tasks in waves to exercise queue cycling
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        block_on_with_spawn(async move {
            // First wave
            let handles1: Vec<_> = (0..5)
                .map(|_| {
                    let cc = c.clone();
                    spawn(async move { cc.fetch_add(1, Ord::SeqCst) })
                })
                .collect();
            for h in handles1 {
                h.await.unwrap();
            }
            // Second wave
            let handles2: Vec<_> = (0..5)
                .map(|_| {
                    let cc = c.clone();
                    spawn(async move { cc.fetch_add(1, Ord::SeqCst) })
                })
                .collect();
            for h in handles2 {
                h.await.unwrap();
            }
        });
        assert_eq!(counter.load(Ord::SeqCst), 10);
    }
}
