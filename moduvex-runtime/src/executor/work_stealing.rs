//! Opt-in work-stealing layer.
//!
//! `StealableQueue` wraps a `LocalQueue` and exposes a `steal_from` method
//! that lets another worker grab half of its tasks. This module is intentionally
//! minimal — the Chase-Lev deque optimisation is deferred to a later phase.
//!
//! For the current single-threaded executor the stealing path is never hot, so
//! correctness and clarity take priority over lock-free performance.

use std::sync::{Arc, Mutex};

use super::scheduler::{GlobalQueue, LocalQueue};

// ── StealableQueue ────────────────────────────────────────────────────────────

/// A `LocalQueue` guarded by a `Mutex` so other workers can steal from it.
///
/// The owning worker holds a `&mut StealableQueue` while running, which gives
/// exclusive (lock-free) access via `local_mut()`. Stealers lock the mutex only
/// when attempting to steal, which is the infrequent path.
pub(crate) struct StealableQueue {
    inner: Mutex<LocalQueue>,
}

impl StealableQueue {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(LocalQueue::new()),
        }
    }

    /// Exclusive mutable access for the owning worker.
    ///
    /// # Panics
    /// Panics if the mutex is poisoned (i.e. a previous worker thread panicked
    /// while holding the lock). This is a non-recoverable programming error.
    pub(crate) fn local_mut(&self) -> std::sync::MutexGuard<'_, LocalQueue> {
        self.inner.lock().unwrap()
    }

    /// Steal up to half of the tasks in this queue into `dest_local`.
    ///
    /// Returns the number of tasks actually stolen. Returns 0 if the queue is
    /// empty or if the destination local queue overflows (unlikely given the
    /// 256-slot capacity).
    pub(crate) fn steal_from(
        &self,
        dest_local: &mut LocalQueue,
        dest_global: &Arc<GlobalQueue>,
    ) -> usize {
        let mut src = self.inner.lock().unwrap();
        let count = src.len() / 2;
        if count == 0 {
            return 0;
        }

        let mut batch = Vec::with_capacity(count);
        src.drain_front(&mut batch, count);
        drop(src); // release lock before pushing to dest

        let mut stolen = 0;
        for header in batch {
            // Try local first; spill overflow to global.
            if let Some(overflow) = dest_local.push(header) {
                dest_global.push_header(overflow);
            }
            stolen += 1;
        }
        stolen
    }

    /// Number of tasks currently in the queue (acquires the lock briefly).
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// `true` if the queue is empty (acquires the lock briefly).
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── WorkStealingPool ──────────────────────────────────────────────────────────

/// Registry of per-worker `StealableQueue`s that enables cross-worker stealing.
///
/// In the current single-threaded executor this pool has exactly one entry.
/// Multi-worker support (spawning N threads each with their own worker) will
/// populate this pool with N entries and use random victim selection.
pub(crate) struct WorkStealingPool {
    queues: Vec<Arc<StealableQueue>>,
}

impl WorkStealingPool {
    pub(crate) fn new() -> Self {
        Self { queues: Vec::new() }
    }

    /// Register a worker's queue with the pool.
    pub(crate) fn add_worker(&mut self, queue: Arc<StealableQueue>) {
        self.queues.push(queue);
    }

    /// Attempt to steal from any worker other than `self_idx`.
    ///
    /// Uses a simple linear scan (no randomisation needed for single-worker).
    /// Returns the number of tasks stolen, or 0 if all queues were empty.
    pub(crate) fn steal_one(
        &self,
        self_idx: usize,
        dest_local: &mut LocalQueue,
        dest_global: &Arc<GlobalQueue>,
    ) -> usize {
        for (idx, queue) in self.queues.iter().enumerate() {
            if idx == self_idx {
                continue;
            }
            let n = queue.steal_from(dest_local, dest_global);
            if n > 0 {
                return n;
            }
        }
        0
    }

    /// Number of registered workers.
    pub(crate) fn worker_count(&self) -> usize {
        self.queues.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::task::{Task, TaskHeader};

    fn make_header() -> Arc<TaskHeader> {
        let (task, _jh) = Task::new(async { 0u32 });
        Arc::clone(&task.header)
    }

    #[test]
    fn steal_from_empty_returns_zero() {
        let src = StealableQueue::new();
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        assert_eq!(src.steal_from(&mut dest, &gq), 0);
    }

    #[test]
    fn steal_from_takes_half() {
        let src = StealableQueue::new();
        {
            let mut local = src.local_mut();
            for _ in 0..8 {
                local.push(make_header());
            }
        }
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        let stolen = src.steal_from(&mut dest, &gq);
        assert_eq!(stolen, 4, "should steal exactly half of 8");
        assert_eq!(src.len(), 4, "source should retain the other half");
    }

    #[test]
    fn pool_steal_skips_self() {
        let q0 = Arc::new(StealableQueue::new());
        let q1 = Arc::new(StealableQueue::new());
        {
            let mut local = q1.local_mut();
            for _ in 0..4 {
                local.push(make_header());
            }
        }
        let mut pool = WorkStealingPool::new();
        pool.add_worker(Arc::clone(&q0));
        pool.add_worker(Arc::clone(&q1));

        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        // Worker 0 tries to steal; skips itself (idx=0), steals from q1 (idx=1).
        let n = pool.steal_one(0, &mut dest, &gq);
        assert!(n >= 1, "should steal from q1");
        assert_eq!(q0.len(), 0, "worker 0's own queue untouched");
    }

    #[test]
    fn local_mut_exclusive_access() {
        let sq = StealableQueue::new();
        {
            let mut local = sq.local_mut();
            assert!(local.push(make_header()).is_none());
            assert_eq!(local.len(), 1);
        }
        assert_eq!(sq.len(), 1);
    }

    // ── Additional work-stealing tests ────────────────────────────────────

    #[test]
    fn steal_from_single_item_queue_returns_zero() {
        // Half of 1 = 0 (integer division) — returns 0 stolen
        let src = StealableQueue::new();
        src.local_mut().push(make_header());
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        let stolen = src.steal_from(&mut dest, &gq);
        assert_eq!(stolen, 0, "can't steal half of 1 task");
    }

    #[test]
    fn stealable_queue_is_empty_and_len() {
        let sq = StealableQueue::new();
        assert!(sq.is_empty());
        assert_eq!(sq.len(), 0);
        sq.local_mut().push(make_header());
        assert!(!sq.is_empty());
        assert_eq!(sq.len(), 1);
    }

    #[test]
    fn pool_worker_count() {
        let mut pool = WorkStealingPool::new();
        assert_eq!(pool.worker_count(), 0);
        pool.add_worker(Arc::new(StealableQueue::new()));
        assert_eq!(pool.worker_count(), 1);
        pool.add_worker(Arc::new(StealableQueue::new()));
        assert_eq!(pool.worker_count(), 2);
    }

    #[test]
    fn pool_all_empty_returns_zero() {
        let mut pool = WorkStealingPool::new();
        pool.add_worker(Arc::new(StealableQueue::new()));
        pool.add_worker(Arc::new(StealableQueue::new()));
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        assert_eq!(pool.steal_one(0, &mut dest, &gq), 0);
    }

    #[test]
    fn steal_many_items_distributes_half() {
        let src = StealableQueue::new();
        {
            let mut local = src.local_mut();
            for _ in 0..20 {
                local.push(make_header());
            }
        }
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        let stolen = src.steal_from(&mut dest, &gq);
        assert_eq!(stolen, 10);
        assert_eq!(src.len(), 10);
    }

    #[test]
    fn pool_steal_only_from_non_empty_worker() {
        let q0 = Arc::new(StealableQueue::new()); // empty
        let q1 = Arc::new(StealableQueue::new()); // empty
        let q2 = Arc::new(StealableQueue::new()); // has tasks
        for _ in 0..4 {
            q2.local_mut().push(make_header());
        }
        let mut pool = WorkStealingPool::new();
        pool.add_worker(Arc::clone(&q0));
        pool.add_worker(Arc::clone(&q1));
        pool.add_worker(Arc::clone(&q2));

        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        // Worker 0 steals; q1 is empty so steals from q2
        let n = pool.steal_one(0, &mut dest, &gq);
        assert!(n >= 1, "should steal from q2");
        assert_eq!(q0.len(), 0);
        assert_eq!(q1.len(), 0);
    }

    #[test]
    fn steal_from_2_items_steals_1() {
        let src = StealableQueue::new();
        src.local_mut().push(make_header());
        src.local_mut().push(make_header());
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        let stolen = src.steal_from(&mut dest, &gq);
        assert_eq!(stolen, 1);
        assert_eq!(src.len(), 1);
    }

    #[test]
    fn stealable_queue_len_after_pop() {
        let sq = StealableQueue::new();
        sq.local_mut().push(make_header());
        sq.local_mut().push(make_header());
        assert_eq!(sq.len(), 2);
        sq.local_mut().pop();
        assert_eq!(sq.len(), 1);
        sq.local_mut().pop();
        assert_eq!(sq.len(), 0);
        assert!(sq.is_empty());
    }

    #[test]
    fn pool_new_has_zero_workers() {
        let pool = WorkStealingPool::new();
        assert_eq!(pool.worker_count(), 0);
    }

    #[test]
    fn pool_steal_one_no_workers_returns_zero() {
        let pool = WorkStealingPool::new();
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        // No workers at all
        assert_eq!(pool.steal_one(0, &mut dest, &gq), 0);
    }

    #[test]
    fn pool_steal_skips_self_when_self_has_work() {
        let q0 = Arc::new(StealableQueue::new()); // has work — but is self
        let q1 = Arc::new(StealableQueue::new()); // empty
        for _ in 0..8 {
            q0.local_mut().push(make_header());
        }
        let mut pool = WorkStealingPool::new();
        pool.add_worker(Arc::clone(&q0));
        pool.add_worker(Arc::clone(&q1));

        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        // Worker 0 tries to steal but only q1 (empty) is eligible
        let n = pool.steal_one(0, &mut dest, &gq);
        assert_eq!(n, 0, "q1 is empty; should not steal from self");
        assert_eq!(q0.len(), 8, "q0 unchanged");
    }

    #[test]
    fn steal_from_6_items_steals_3() {
        let src = StealableQueue::new();
        for _ in 0..6 {
            src.local_mut().push(make_header());
        }
        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        let stolen = src.steal_from(&mut dest, &gq);
        assert_eq!(stolen, 3);
        assert_eq!(src.len(), 3);
    }

    #[test]
    fn pool_steal_one_worker_2_non_self_returns_from_second() {
        let q0 = Arc::new(StealableQueue::new());
        let q1 = Arc::new(StealableQueue::new());
        // Give q1 some work
        for _ in 0..4 {
            q1.local_mut().push(make_header());
        }
        let mut pool = WorkStealingPool::new();
        pool.add_worker(Arc::clone(&q0));
        pool.add_worker(Arc::clone(&q1));

        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        let n = pool.steal_one(0, &mut dest, &gq);
        assert_eq!(n, 2, "should steal 2 from q1 (half of 4)");
        assert_eq!(q1.len(), 2);
    }

    #[test]
    fn stealable_queue_push_16_items() {
        let sq = StealableQueue::new();
        for _ in 0..16 {
            sq.local_mut().push(make_header());
        }
        assert_eq!(sq.len(), 16);
        assert!(!sq.is_empty());
    }

    #[test]
    fn pool_3_workers_steal_from_last() {
        let q0 = Arc::new(StealableQueue::new());
        let q1 = Arc::new(StealableQueue::new());
        let q2 = Arc::new(StealableQueue::new());
        for _ in 0..10 {
            q2.local_mut().push(make_header());
        }
        let mut pool = WorkStealingPool::new();
        pool.add_worker(Arc::clone(&q0));
        pool.add_worker(Arc::clone(&q1));
        pool.add_worker(Arc::clone(&q2));

        let mut dest = LocalQueue::new();
        let gq = Arc::new(GlobalQueue::new());
        let n = pool.steal_one(0, &mut dest, &gq);
        // q0=empty(skip self), q1=empty, q2=10 items → steal 5
        assert_eq!(n, 5);
        assert_eq!(q2.len(), 5);
    }
}
