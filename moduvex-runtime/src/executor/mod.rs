//! Single-threaded async executor with work-stealing extension points.
//!
//! # Run loop
//! ```text
//! block_on(future)
//!   └─ Executor::run_loop
//!        ├─ LocalQueue  (LIFO ring, 256 slots)
//!        ├─ GlobalQueue (Mutex<VecDeque> — waker injection)
//!        └─ Reactor     (kqueue/epoll — parks when no work is ready)
//! ```
//!
//! The loop runs until the *root* future (the argument to `block_on`) resolves.
//! Spawned tasks are driven as side-effects of the same loop.

pub mod scheduler;
pub mod task;
pub mod task_local;
pub mod waker;
pub mod work_stealing;

use std::cell::Cell;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::platform::sys::{create_pipe, events_with_capacity, Interest};
use crate::reactor::{with_reactor, with_reactor_mut};
use crate::time::{next_timer_deadline, tick_timer_wheel};

use scheduler::{GlobalQueue, LocalQueue};
use task::{JoinHandle, Task, STATE_CANCELLED, STATE_COMPLETED};
use waker::make_waker;

// ── Executor ──────────────────────────────────────────────────────────────────

/// Per-thread async executor.
pub struct Executor {
    /// LIFO local task queue — popped first each iteration.
    local: LocalQueue,
    /// Shared with all `Waker`s — they push here to re-schedule tasks.
    global: Arc<GlobalQueue>,
    /// Owned `Task` handles keyed by `Arc<TaskHeader>` pointer address.
    /// The executor must own `Task` to manage the future/body lifetime.
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
            // ── 1. Tick timer wheel — wake expired timers ─────────────────
            let expired = tick_timer_wheel(std::time::Instant::now());
            for w in expired {
                w.wake();
            }

            // ── 2. Poll root ──────────────────────────────────────────────
            if !root_done {
                let mut cx = Context::from_waker(&root_waker);
                if let Poll::Ready(val) = root.as_mut().poll(&mut cx) {
                    root_output = Some(val);
                    root_done = true;
                }
            }

            // ── 3. Exit if root done and no spawned tasks remain ──────────
            if root_done && self.tasks.is_empty() {
                break;
            }

            // ── 4. Drain task queues ──────────────────────────────────────
            let mut did_work = false;
            loop {
                let Some(header) = self.next_task() else {
                    break;
                };
                did_work = true;
                let key = Arc::as_ptr(&header) as usize;
                let state = header.state.load(Ordering::Acquire);

                if state == STATE_CANCELLED {
                    // Call cancel() so the body is freed and join_waker woken.
                    if let Some(task) = self.tasks.remove(&key) {
                        task.cancel();
                    }
                    continue;
                }
                if state == STATE_COMPLETED {
                    // poll_task already freed the body; just drop ownership.
                    self.tasks.remove(&key);
                    continue;
                }

                // Build a waker that re-enqueues this header on wake().
                let waker = make_waker(Arc::clone(&header), Arc::clone(&self.global));
                let mut cx = Context::from_waker(&waker);

                if let Some(task) = self.tasks.get(&key) {
                    let completed = task.poll_task(&mut cx);
                    if completed {
                        self.tasks.remove(&key);
                    }
                }
                // else: already removed in a previous pass — harmless
            }

            // ── 5. Re-check exit after draining ──────────────────────────
            if root_done && self.tasks.is_empty() {
                break;
            }

            // ── 6. Park on reactor when both queues empty ─────────────────
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
    ///
    /// If no timers are pending, parks for at most `MAX_PARK_MS` milliseconds.
    /// This ensures the run loop wakes up to tick expired timers promptly.
    fn park(&self) {
        const MAX_PARK_MS: u64 = 10;

        // Compute how long to block: clamp to time-until-next-timer-deadline.
        let timeout_ms = match next_timer_deadline() {
            None => MAX_PARK_MS,
            Some(deadline) => {
                let now = std::time::Instant::now();
                if deadline <= now {
                    0 // deadline already passed — don't block
                } else {
                    let ms = deadline.duration_since(now).as_millis() as u64;
                    ms.min(MAX_PARK_MS)
                }
            }
        };

        let mut events = events_with_capacity(64);
        // poll() also fires I/O wakers stored in the waker registry.
        let _ = with_reactor_mut(|r| r.poll(&mut events, Some(timeout_ms)));
        self.drain_wake_pipe();
    }

    /// Read all pending bytes from the self-pipe's read end (non-blocking).
    fn drain_wake_pipe(&self) {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: `wake_rx` is a valid O_NONBLOCK fd we own.
            let n = unsafe { libc::read(self.wake_rx, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            } // EAGAIN (-1) or EOF (0)
        }
    }

    /// Build a `Waker` for the root future. On wake, writes one byte to the
    /// self-pipe so the reactor's `poll` returns immediately.
    fn make_root_waker(&self) -> std::task::Waker {
        use std::task::{RawWaker, RawWakerVTable};

        let tx = self.wake_tx;

        // These four functions implement the RawWaker contract for the root waker.
        // The data pointer encodes the pipe write-fd as a usize (no heap alloc).
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
        unsafe fn drop_root(_: *const ()) {} // no heap allocation to free

        static ROOT_VTABLE: RawWakerVTable =
            RawWakerVTable::new(clone_root, wake_root, wake_root_by_ref, drop_root);

        let raw = std::task::RawWaker::new(tx as usize as *const (), &ROOT_VTABLE);
        // SAFETY: ROOT_VTABLE satisfies the RawWaker contract; the fd lives for
        // the duration of the Executor which outlives any root poll call.
        unsafe { std::task::Waker::from_raw(raw) }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        let _ = with_reactor(|r| r.deregister(self.wake_rx));
        // SAFETY: we own wake_rx and wake_tx exclusively.
        unsafe {
            libc::close(self.wake_rx);
            libc::close(self.wake_tx);
        }
    }
}

/// Sentinel reactor token for the self-pipe read end (must not collide with
/// user tokens, which start at 0).
const WAKE_TOKEN: usize = usize::MAX;

// ── Thread-local executor pointer ─────────────────────────────────────────────

thread_local! {
    /// Raw pointer to the current thread's `Executor`.
    /// Non-null only inside a `block_on_with_spawn` call.
    static CURRENT_EXECUTOR: Cell<*mut Executor> = const { Cell::new(std::ptr::null_mut()) };
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Drive `future` to completion on the current thread, returning its output.
///
/// # Panics
/// Panics if the executor fails to initialise (kqueue/pipe failure).
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut exec = Executor::new().expect("executor init failed");
    exec.block_on(future)
}

/// Drive `future` to completion with `spawn()` available in the async context.
///
/// Registers the executor as a thread-local so that `spawn()` works inside the
/// future. Clears the thread-local before returning.
pub fn block_on_with_spawn<F: Future>(future: F) -> F::Output {
    let mut exec = Executor::new().expect("executor init failed");
    CURRENT_EXECUTOR.with(|c| c.set(&mut exec as *mut Executor));
    let result = exec.block_on(future);
    CURRENT_EXECUTOR.with(|c| c.set(std::ptr::null_mut()));
    result
}

/// Spawn a future onto the current thread's executor.
///
/// # Panics
/// Panics if called outside of a `block_on_with_spawn` context.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: Send + 'static,
{
    CURRENT_EXECUTOR.with(|cell| {
        let ptr = cell.get();
        assert!(
            !ptr.is_null(),
            "spawn() called outside of block_on_with_spawn context"
        );
        // SAFETY: ptr is valid for the duration of `block_on_with_spawn`, which
        // runs the entire async tree (including this call) on the same thread.
        unsafe { (*ptr).spawn(future) }
    })
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
        // Verify that nested spawns complete before the root exits.
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
}
