//! Per-core task queues: `LocalQueue` (ring buffer) + `GlobalQueue` (mutex deque).
//!
//! # Design
//! - `LocalQueue` — fixed-capacity 256-slot ring buffer for LIFO local dequeuing.
//!   Overflow tasks (when ring is full) are spilled to the `GlobalQueue`.
//! - `GlobalQueue` — `Mutex<VecDeque<Arc<TaskHeader>>>` for cross-thread injection
//!   and work-stealing. Also stores `Task` handles for executor ownership.
//!
//! Both queues operate on `Arc<TaskHeader>` for the waker path, and `Task` for
//! the executor-ownership path. The distinction matters for drop semantics:
//! - Wakers push `Arc<TaskHeader>` (no Future ownership).
//! - Executor pops `Arc<TaskHeader>` and looks up its owned `Task` by pointer.
//!
//! For simplicity in the single-threaded executor, both queues store
//! `Arc<TaskHeader>` and the executor maintains a separate slab of `Task` owners.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::task::TaskHeader;

// ── LocalQueue ────────────────────────────────────────────────────────────────

/// Fixed-size ring-buffer local queue (256 slots).
///
/// Operates as a LIFO stack for cache-friendly reuse of recently-scheduled
/// tasks. When full, `push` returns the overflow item for the caller to spill
/// to the global queue.
pub(crate) struct LocalQueue {
    /// Ring buffer storage. `head` is the next pop index; `tail` is the next
    /// push index. Full when `(tail - head) == CAPACITY`.
    buf: Box<[Option<Arc<TaskHeader>>; CAPACITY]>,
    head: usize,
    tail: usize,
}

const CAPACITY: usize = 256;

impl LocalQueue {
    pub(crate) fn new() -> Self {
        // SAFETY: Option<Arc<TaskHeader>> is safely zero-initialised as None
        // via the MaybeUninit → assume_init pattern below.
        let buf = {
            // Box<[Option<Arc<TaskHeader>>; CAPACITY]> cannot be created with
            // a const initialiser because Arc is not Copy. Use a vec-based
            // approach instead.
            let v: Vec<Option<Arc<TaskHeader>>> = (0..CAPACITY).map(|_| None).collect();
            // Convert Vec → Box<[_; CAPACITY]>.
            let boxed_slice = v.into_boxed_slice();
            // SAFETY: We constructed exactly CAPACITY elements above.
            unsafe {
                Box::from_raw(Box::into_raw(boxed_slice) as *mut [Option<Arc<TaskHeader>>; CAPACITY])
            }
        };
        Self {
            buf,
            head: 0,
            tail: 0,
        }
    }

    /// Number of items currently held.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.tail.wrapping_sub(self.head)
    }

    /// `true` if the queue holds no items.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `true` when the ring buffer is at capacity.
    #[inline]
    fn is_full(&self) -> bool {
        self.len() == CAPACITY
    }

    /// Push `header` onto the local queue.
    ///
    /// Returns `Some(header)` if the queue was full (caller must spill to
    /// global), `None` on success.
    pub(crate) fn push(&mut self, header: Arc<TaskHeader>) -> Option<Arc<TaskHeader>> {
        if self.is_full() {
            return Some(header);
        }
        let idx = self.tail % CAPACITY;
        self.buf[idx] = Some(header);
        self.tail = self.tail.wrapping_add(1);
        None
    }

    /// Pop the most-recently-pushed item (LIFO).
    pub(crate) fn pop(&mut self) -> Option<Arc<TaskHeader>> {
        if self.is_empty() {
            return None;
        }
        // Decrement tail for LIFO behaviour.
        self.tail = self.tail.wrapping_sub(1);
        let idx = self.tail % CAPACITY;
        self.buf[idx].take()
    }

    /// Drain up to `count` items from the front (FIFO) into `dest`.
    /// Used by the work-stealer to grab a batch.
    pub(crate) fn drain_front(&mut self, dest: &mut Vec<Arc<TaskHeader>>, count: usize) {
        let to_take = count.min(self.len());
        for _ in 0..to_take {
            let idx = self.head % CAPACITY;
            if let Some(item) = self.buf[idx].take() {
                dest.push(item);
            }
            self.head = self.head.wrapping_add(1);
        }
    }
}

// ── GlobalQueue ───────────────────────────────────────────────────────────────

/// Cross-thread injection queue for wakers and stolen tasks.
///
/// Guarded by a `Mutex`; only accessed when the local queue is empty or a
/// waker fires from another thread.
pub(crate) struct GlobalQueue {
    inner: Mutex<VecDeque<Arc<TaskHeader>>>,
}

impl GlobalQueue {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
        }
    }

    /// Append `header` to the back of the queue.
    pub(crate) fn push_header(&self, header: Arc<TaskHeader>) {
        // Unwrap: only fails if the mutex is poisoned, which is a programming error.
        self.inner.lock().unwrap().push_back(header);
    }

    /// Remove and return the front item, or `None` if empty.
    pub(crate) fn pop(&self) -> Option<Arc<TaskHeader>> {
        self.inner.lock().unwrap().pop_front()
    }

    /// Steal up to half the queue's contents into `local`.
    ///
    /// Returns the number of tasks stolen.
    pub(crate) fn steal_batch(&self, local: &mut LocalQueue) -> usize {
        let mut guard = self.inner.lock().unwrap();
        let count = (guard.len() / 2).max(1).min(guard.len());
        let mut stolen = 0;
        for _ in 0..count {
            match guard.pop_front() {
                Some(h) => {
                    if local.push(h).is_none() {
                        stolen += 1;
                    }
                    // If local overflows, stop stealing.
                    else {
                        break;
                    }
                }
                None => break,
            }
        }
        stolen
    }

    /// Number of items waiting in the global queue.
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::task::Task;

    fn make_header() -> Arc<TaskHeader> {
        let (task, _jh) = Task::new(async { 0u32 });
        Arc::clone(&task.header)
        // task drops here but header Arc stays alive
    }

    // --- LocalQueue tests ---

    #[test]
    fn local_queue_push_pop_lifo() {
        let mut q = LocalQueue::new();
        let h1 = make_header();
        let h2 = make_header();
        let p1 = Arc::as_ptr(&h1);
        let p2 = Arc::as_ptr(&h2);
        assert!(q.push(h1).is_none());
        assert!(q.push(h2).is_none());
        // LIFO: last in, first out
        assert_eq!(Arc::as_ptr(&q.pop().unwrap()), p2);
        assert_eq!(Arc::as_ptr(&q.pop().unwrap()), p1);
        assert!(q.pop().is_none());
    }

    #[test]
    fn local_queue_overflow_returns_item() {
        let mut q = LocalQueue::new();
        // Fill to capacity
        for _ in 0..CAPACITY {
            assert!(q.push(make_header()).is_none());
        }
        assert!(q.is_full());
        let overflow = q.push(make_header());
        assert!(overflow.is_some(), "full queue must return overflow item");
    }

    #[test]
    fn local_queue_drain_front() {
        let mut q = LocalQueue::new();
        for _ in 0..6 {
            q.push(make_header());
        }
        let mut dest = Vec::new();
        q.drain_front(&mut dest, 3);
        assert_eq!(dest.len(), 3);
        assert_eq!(q.len(), 3);
    }

    // --- GlobalQueue tests ---

    #[test]
    fn global_queue_push_pop() {
        let gq = GlobalQueue::new();
        let h = make_header();
        let p = Arc::as_ptr(&h);
        gq.push_header(h);
        let popped = gq.pop().unwrap();
        assert_eq!(Arc::as_ptr(&popped), p);
        assert!(gq.pop().is_none());
    }

    #[test]
    fn global_queue_steal_batch_half() {
        let gq = GlobalQueue::new();
        for _ in 0..8 {
            gq.push_header(make_header());
        }
        let mut local = LocalQueue::new();
        let stolen = gq.steal_batch(&mut local);
        assert!(
            (1..=4).contains(&stolen),
            "should steal ~half: got {stolen}"
        );
        assert_eq!(local.len(), stolen);
    }

    // ── Additional scheduler tests ─────────────────────────────────────────

    #[test]
    fn local_queue_empty_on_new() {
        let q = LocalQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn local_queue_pop_empty_returns_none() {
        let mut q = LocalQueue::new();
        assert!(q.pop().is_none());
    }

    #[test]
    fn local_queue_len_increments_on_push() {
        let mut q = LocalQueue::new();
        for i in 0..5 {
            assert_eq!(q.len(), i);
            assert!(q.push(make_header()).is_none());
            assert_eq!(q.len(), i + 1);
        }
    }

    #[test]
    fn local_queue_drain_front_empty_is_noop() {
        let mut q = LocalQueue::new();
        let mut dest = Vec::new();
        q.drain_front(&mut dest, 10);
        assert!(dest.is_empty());
    }

    #[test]
    fn local_queue_drain_front_more_than_len_drains_all() {
        let mut q = LocalQueue::new();
        for _ in 0..3 {
            q.push(make_header());
        }
        let mut dest = Vec::new();
        q.drain_front(&mut dest, 100);
        assert_eq!(dest.len(), 3);
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn global_queue_empty_pop_returns_none() {
        let gq = GlobalQueue::new();
        assert!(gq.pop().is_none());
    }

    #[test]
    fn global_queue_len_tracks_count() {
        let gq = GlobalQueue::new();
        assert_eq!(gq.len(), 0);
        gq.push_header(make_header());
        assert_eq!(gq.len(), 1);
        gq.push_header(make_header());
        assert_eq!(gq.len(), 2);
        let _ = gq.pop();
        assert_eq!(gq.len(), 1);
    }

    #[test]
    fn global_queue_fifo_ordering() {
        let gq = GlobalQueue::new();
        let h1 = make_header();
        let h2 = make_header();
        let p1 = Arc::as_ptr(&h1);
        let p2 = Arc::as_ptr(&h2);
        gq.push_header(h1);
        gq.push_header(h2);
        // FIFO: first in, first out
        assert_eq!(Arc::as_ptr(&gq.pop().unwrap()), p1);
        assert_eq!(Arc::as_ptr(&gq.pop().unwrap()), p2);
    }

    #[test]
    fn global_queue_steal_batch_single_item_returns_one() {
        let gq = GlobalQueue::new();
        gq.push_header(make_header());
        let mut local = LocalQueue::new();
        let stolen = gq.steal_batch(&mut local);
        assert_eq!(stolen, 1);
        assert_eq!(gq.len(), 0);
    }

    #[test]
    fn global_queue_steal_batch_empty_returns_zero() {
        let gq = GlobalQueue::new();
        let mut local = LocalQueue::new();
        let stolen = gq.steal_batch(&mut local);
        assert_eq!(stolen, 0);
    }

    #[test]
    fn local_queue_push_many_pop_all() {
        let mut q = LocalQueue::new();
        for _ in 0..10 {
            q.push(make_header());
        }
        assert_eq!(q.len(), 10);
        let mut count = 0;
        while q.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, 10);
        assert!(q.is_empty());
    }

    #[test]
    fn global_queue_push_many_pop_in_fifo_order() {
        let gq = GlobalQueue::new();
        let mut ptrs = Vec::new();
        for _ in 0..5 {
            let h = make_header();
            ptrs.push(Arc::as_ptr(&h));
            gq.push_header(h);
        }
        for ptr in ptrs {
            let popped = gq.pop().unwrap();
            assert_eq!(Arc::as_ptr(&popped), ptr);
        }
        assert!(gq.pop().is_none());
    }

    #[test]
    fn local_queue_interleaved_push_pop() {
        let mut q = LocalQueue::new();
        q.push(make_header());
        q.push(make_header());
        q.pop();
        assert_eq!(q.len(), 1);
        q.push(make_header());
        q.push(make_header());
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn global_queue_steal_batch_10_items_steals_at_least_1() {
        let gq = GlobalQueue::new();
        for _ in 0..10 {
            gq.push_header(make_header());
        }
        let mut local = LocalQueue::new();
        let stolen = gq.steal_batch(&mut local);
        assert!(stolen >= 1);
        assert!(stolen <= 5); // at most half
    }

    #[test]
    fn local_queue_is_not_empty_after_push() {
        let mut q = LocalQueue::new();
        assert!(q.is_empty());
        q.push(make_header());
        assert!(!q.is_empty());
    }

    #[test]
    fn local_queue_push_then_pop_lifo_2_items() {
        let mut q = LocalQueue::new();
        let h1 = make_header();
        let h2 = make_header();
        let p1 = Arc::as_ptr(&h1);
        let p2 = Arc::as_ptr(&h2);
        q.push(h1);
        q.push(h2);
        // LIFO: second pushed is popped first
        assert_eq!(Arc::as_ptr(&q.pop().unwrap()), p2);
        assert_eq!(Arc::as_ptr(&q.pop().unwrap()), p1);
    }

    #[test]
    fn global_queue_multiple_push_pop_cycles() {
        let gq = GlobalQueue::new();
        for _ in 0..3 {
            gq.push_header(make_header());
            gq.push_header(make_header());
            gq.pop();
        }
        assert_eq!(gq.len(), 3);
    }

    #[test]
    fn local_queue_drain_front_partial() {
        let mut q = LocalQueue::new();
        for _ in 0..10 {
            q.push(make_header());
        }
        let mut dest = Vec::new();
        q.drain_front(&mut dest, 4);
        assert_eq!(dest.len(), 4);
        assert_eq!(q.len(), 6);
    }

    #[test]
    fn global_queue_steal_batch_large_queue() {
        let gq = GlobalQueue::new();
        for _ in 0..100 {
            gq.push_header(make_header());
        }
        let mut local = LocalQueue::new();
        let stolen = gq.steal_batch(&mut local);
        assert!(stolen >= 1);
        assert!(stolen <= 50);
    }
}
