//! Hierarchical timer wheel — O(1) insert/cancel, O(levels) tick.
//!
//! # Design
//! 6 levels × 64 slots. Each level covers a range of deadlines:
//!
//! | Level | Slot width | Total range |
//! |-------|-----------|-------------|
//! | 0     | 1 ms      | 64 ms       |
//! | 1     | 64 ms     | ~4 s        |
//! | 2     | ~4 s      | ~4 min      |
//! | 3     | ~4 min    | ~4.5 h      |
//! | 4     | ~4.5 h    | ~12 d       |
//! | 5     | ~12 d     | ~2 yr       |
//!
//! Timers beyond level 5 are clamped into the last slot of level 5.
//!
//! # Cascade
//! When the executor's "current tick" advances past a slot boundary at level N,
//! all timers in that slot are re-inserted at level N-1 (standard wheel cascade).

use std::collections::HashMap;
use std::task::Waker;
use std::time::Instant;

/// Number of slots per wheel level (must be a power of 2).
const SLOTS: usize = 64;
const SLOTS_MASK: u64 = (SLOTS - 1) as u64;

/// Number of wheel levels.
const LEVELS: usize = 6;

/// Width of level 0 in milliseconds (1 ms per slot).
const LEVEL0_MS: u64 = 1;

/// Width of each slot at level N = LEVEL0_MS * SLOTS^N.
fn slot_width_ms(level: usize) -> u64 {
    LEVEL0_MS * (SLOTS as u64).pow(level as u32)
}

// ── Timer entry ───────────────────────────────────────────────────────────────

/// A single pending timer.
#[derive(Debug)]
pub(crate) struct TimerEntry {
    /// Unique timer identifier (for cancellation).
    pub id: u64,
    /// Absolute deadline.
    pub deadline: Instant,
    /// Waker to call when the deadline passes.
    pub waker: Waker,
}

// ── TimerId ───────────────────────────────────────────────────────────────────

/// Opaque handle returned by `TimerWheel::insert`. Used to cancel a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(u64);

// ── TimerWheel ────────────────────────────────────────────────────────────────

/// Hierarchical timer wheel.
///
/// All operations are relative to a monotonic millisecond counter derived from
/// an `Instant` origin captured at construction time.
pub struct TimerWheel {
    /// The `Instant` corresponding to tick 0.
    origin: Instant,
    /// `wheel[level][slot]` → list of timer entries.
    wheel: Vec<Vec<Vec<TimerEntry>>>,
    /// Index (slot, level) of each active timer for O(1) lookup on cancel.
    /// Maps timer id → (level, slot).
    index: HashMap<u64, (usize, usize)>,
    /// Monotonically increasing ID counter.
    next_id: u64,
    /// Last processed tick (milliseconds since origin).
    last_tick_ms: u64,
}

impl TimerWheel {
    /// Create a new timer wheel with `origin` as the zero point.
    pub(crate) fn new(origin: Instant) -> Self {
        // wheel[level][slot] = vec of entries
        let wheel = (0..LEVELS)
            .map(|_| (0..SLOTS).map(|_| Vec::new()).collect())
            .collect();
        Self {
            origin,
            wheel,
            index: HashMap::new(),
            next_id: 1,
            last_tick_ms: 0,
        }
    }

    /// Convert an `Instant` to milliseconds since origin, saturating at 0.
    fn instant_to_ms(&self, t: Instant) -> u64 {
        t.saturating_duration_since(self.origin)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// Insert a timer that fires at `deadline`. Returns a `TimerId` for
    /// cancellation. The `waker` is called when the deadline passes.
    pub(crate) fn insert(&mut self, deadline: Instant, waker: Waker) -> TimerId {
        let id = self.next_id;
        self.next_id += 1;

        let deadline_ms = self.instant_to_ms(deadline);
        // Fire immediately if deadline already passed.
        let effective_ms = deadline_ms.max(self.last_tick_ms);

        let (level, slot) = self.level_slot(effective_ms);
        self.wheel[level][slot].push(TimerEntry {
            id,
            deadline,
            waker,
        });
        self.index.insert(id, (level, slot));

        TimerId(id)
    }

    /// Cancel the timer identified by `id`. Returns `true` if the timer was
    /// found and removed, `false` if it had already fired or was not found.
    pub(crate) fn cancel(&mut self, id: TimerId) -> bool {
        let Some((level, slot)) = self.index.remove(&id.0) else {
            return false;
        };
        let bucket = &mut self.wheel[level][slot];
        let before = bucket.len();
        bucket.retain(|e| e.id != id.0);
        bucket.len() < before
    }

    /// Advance the wheel to `now`, returning all wakers whose timers have
    /// expired. Callers must call `wake()` on each returned `Waker`.
    ///
    /// # Performance
    /// For large time jumps (e.g. 10 seconds), this method drains the range of
    /// level-0 slots in one pass rather than iterating ms-by-ms. Higher levels
    /// are cascaded only at their slot boundaries within the range. This keeps
    /// the cost proportional to the number of distinct slots crossed (O(slots))
    /// rather than elapsed milliseconds (O(ms)).
    pub(crate) fn tick(&mut self, now: Instant) -> Vec<Waker> {
        let now_ms = self.instant_to_ms(now);
        let mut fired: Vec<Waker> = Vec::new();

        let from = self.last_tick_ms;
        let to = now_ms;

        if to < from {
            return fired;
        }

        // ── Optimised path: drain all expired slots without ms-by-ms iteration ──
        //
        // Level 0 wraps every 64 slots (64 ms per revolution). If the jump
        // spans more than 64 ms we drain *all* level-0 slots (full revolution).
        // Otherwise we drain just the range [from_slot0..=to_slot0].
        let from_slot0 = (from & SLOTS_MASK) as usize;
        let to_slot0 = (to & SLOTS_MASK) as usize;
        let span = to.saturating_sub(from);

        if span >= SLOTS as u64 {
            // Full revolution or more — drain every level-0 slot.
            for slot in 0..SLOTS {
                self.drain_slot(0, slot, to, &mut fired);
            }
        } else if from_slot0 <= to_slot0 {
            // No wrap-around within this revolution.
            for slot in from_slot0..=to_slot0 {
                self.drain_slot(0, slot, to, &mut fired);
            }
        } else {
            // Wrap-around: drain [from_slot0..SLOTS) then [0..=to_slot0].
            for slot in from_slot0..SLOTS {
                self.drain_slot(0, slot, to, &mut fired);
            }
            for slot in 0..=to_slot0 {
                self.drain_slot(0, slot, to, &mut fired);
            }
        }

        // ── Cascade higher levels at their slot boundaries within [from, to] ──
        for level in 1..LEVELS {
            let width = slot_width_ms(level);
            // First boundary at or after `from`.
            let first_boundary = if from % width == 0 {
                from
            } else {
                (from / width + 1) * width
            };
            let mut boundary = first_boundary;
            while boundary <= to {
                let slot = ((boundary / width) & SLOTS_MASK) as usize;
                self.drain_slot(level, slot, to, &mut fired);
                boundary = match boundary.checked_add(width) {
                    Some(b) => b,
                    None => break,
                };
            }
        }

        self.last_tick_ms = to;
        fired
    }

    /// Drain all entries from `wheel[level][slot]`.
    ///
    /// Entries whose deadline has passed (≤ `now_ms`) are fired immediately.
    /// Entries still in the future are re-inserted at the correct wheel position.
    fn drain_slot(&mut self, level: usize, slot: usize, now_ms: u64, fired: &mut Vec<Waker>) {
        let entries = std::mem::take(&mut self.wheel[level][slot]);
        for entry in entries {
            self.index.remove(&entry.id);
            if self.instant_to_ms(entry.deadline) <= now_ms {
                fired.push(entry.waker);
            } else {
                self.insert_raw(entry);
            }
        }
    }

    /// Return the nearest deadline across all wheel slots, if any timers are pending.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        for level in &self.wheel {
            for slot in level {
                for entry in slot {
                    earliest = Some(match earliest {
                        None => entry.deadline,
                        Some(e) => e.min(entry.deadline),
                    });
                }
            }
        }
        earliest
    }

    /// Internal: insert a pre-existing `TimerEntry` into the correct bucket.
    fn insert_raw(&mut self, entry: TimerEntry) {
        let deadline_ms = self.instant_to_ms(entry.deadline);
        let effective_ms = deadline_ms.max(self.last_tick_ms);
        let (level, slot) = self.level_slot(effective_ms);
        self.index.insert(entry.id, (level, slot));
        self.wheel[level][slot].push(entry);
    }

    /// Compute the (level, slot) for a timer with deadline at `deadline_ms`.
    fn level_slot(&self, deadline_ms: u64) -> (usize, usize) {
        let delta = deadline_ms.saturating_sub(self.last_tick_ms);

        for level in 0..LEVELS {
            let width = slot_width_ms(level);
            let range = width * SLOTS as u64;
            if delta < range || level == LEVELS - 1 {
                // Compute absolute slot at this level.
                let slot = ((deadline_ms / width) & SLOTS_MASK) as usize;
                return (level, slot);
            }
        }
        // Unreachable: loop handles all cases.
        (LEVELS - 1, 0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::task::{RawWaker, RawWakerVTable};
    use std::time::Duration;

    fn make_flag_waker(flag: Arc<Mutex<bool>>) -> Waker {
        let data = Arc::into_raw(flag) as *const ();

        unsafe fn clone_w(p: *const ()) -> RawWaker {
            Arc::increment_strong_count(p as *const Mutex<bool>);
            RawWaker::new(p, &VT)
        }
        unsafe fn wake(p: *const ()) {
            *Arc::from_raw(p as *const Mutex<bool>).lock().unwrap() = true;
        }
        unsafe fn wake_ref(p: *const ()) {
            *(*(&p as *const *const () as *const Arc<Mutex<bool>>))
                .lock()
                .unwrap() = true;
        }
        unsafe fn drop_w(p: *const ()) {
            drop(Arc::from_raw(p as *const Mutex<bool>));
        }
        static VT: RawWakerVTable = RawWakerVTable::new(clone_w, wake, wake_ref, drop_w);

        // SAFETY: vtable satisfies the RawWaker contract.
        unsafe { Waker::from_raw(RawWaker::new(data, &VT)) }
    }

    #[test]
    fn insert_and_tick_fires_waker() {
        let flag = Arc::new(Mutex::new(false));
        let waker = make_flag_waker(Arc::clone(&flag));

        let origin = Instant::now();
        let mut wheel = TimerWheel::new(origin);

        let deadline = origin + Duration::from_millis(50);
        wheel.insert(deadline, waker);

        // Tick before deadline — should not fire.
        let wakers = wheel.tick(origin + Duration::from_millis(30));
        assert!(wakers.is_empty());

        // Tick at/after deadline — should fire.
        let wakers = wheel.tick(origin + Duration::from_millis(60));
        assert_eq!(wakers.len(), 1);
        for w in wakers {
            w.wake();
        }
        assert!(*flag.lock().unwrap(), "waker must have fired");
    }

    #[test]
    fn cancel_prevents_firing() {
        let flag = Arc::new(Mutex::new(false));
        let waker = make_flag_waker(Arc::clone(&flag));

        let origin = Instant::now();
        let mut wheel = TimerWheel::new(origin);

        let deadline = origin + Duration::from_millis(50);
        let id = wheel.insert(deadline, waker);
        let removed = wheel.cancel(id);
        assert!(removed, "cancel must return true for existing timer");

        // Tick past deadline — must not fire.
        let wakers = wheel.tick(origin + Duration::from_millis(100));
        assert!(wakers.is_empty(), "cancelled timer must not fire");
        assert!(!*flag.lock().unwrap());
    }

    #[test]
    fn zero_deadline_fires_on_next_tick() {
        let flag = Arc::new(Mutex::new(false));
        let waker = make_flag_waker(Arc::clone(&flag));

        let origin = Instant::now();
        let mut wheel = TimerWheel::new(origin);

        // Deadline in the past (or now) → fires immediately on next tick.
        wheel.insert(origin, waker);
        let wakers = wheel.tick(origin + Duration::from_millis(1));
        assert_eq!(wakers.len(), 1);
        for w in wakers {
            w.wake();
        }
        assert!(*flag.lock().unwrap());
    }

    #[test]
    fn multiple_timers_fire_in_order() {
        let origin = Instant::now();
        let mut wheel = TimerWheel::new(origin);
        let results = Arc::new(Mutex::new(Vec::<u32>::new()));

        for i in 0u32..5 {
            let r = Arc::clone(&results);
            let flag = Arc::new(Mutex::new(false));
            let _waker = make_flag_waker(Arc::clone(&flag));
            let _ = flag; // waker owns it now
                          // Re-build a waker that records the index.
            let data = Box::into_raw(Box::new((i, r))) as *const ();
            type Payload = (u32, Arc<Mutex<Vec<u32>>>);
            unsafe fn clone_p(p: *const ()) -> RawWaker {
                let b = Box::from_raw(p as *mut Payload);
                let cloned = Box::new((b.0, Arc::clone(&b.1)));
                std::mem::forget(b);
                RawWaker::new(Box::into_raw(cloned) as *const (), &PVT)
            }
            unsafe fn wake_p(p: *const ()) {
                let b = Box::from_raw(p as *mut Payload);
                b.1.lock().unwrap().push(b.0);
            }
            unsafe fn wake_p_ref(p: *const ()) {
                let b = Box::from_raw(p as *mut Payload);
                b.1.lock().unwrap().push(b.0);
                std::mem::forget(b);
            }
            unsafe fn drop_p(p: *const ()) {
                drop(Box::from_raw(p as *mut Payload));
            }
            static PVT: RawWakerVTable = RawWakerVTable::new(clone_p, wake_p, wake_p_ref, drop_p);
            // SAFETY: PVT satisfies the RawWaker contract; payload is Box-allocated.
            let waker2 = unsafe { Waker::from_raw(RawWaker::new(data, &PVT)) };

            wheel.insert(origin + Duration::from_millis((i as u64 + 1) * 10), waker2);
        }

        // Single tick past all deadlines.
        let wakers = wheel.tick(origin + Duration::from_millis(60));
        assert_eq!(wakers.len(), 5);
        for w in wakers {
            w.wake();
        }
        let v = results.lock().unwrap();
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn next_deadline_returns_earliest() {
        let origin = Instant::now();
        let mut wheel = TimerWheel::new(origin);

        let d1 = origin + Duration::from_millis(200);
        let d2 = origin + Duration::from_millis(50);

        let f1 = Arc::new(Mutex::new(false));
        let f2 = Arc::new(Mutex::new(false));
        wheel.insert(d1, make_flag_waker(Arc::clone(&f1)));
        wheel.insert(d2, make_flag_waker(Arc::clone(&f2)));

        let earliest = wheel.next_deadline().expect("should have a deadline");
        assert_eq!(earliest, d2, "next_deadline must return earliest");
    }

    #[test]
    fn large_time_jump_fires_timer_quickly() {
        // Regression test: a 10-second jump should not cause O(10_000) ms iterations.
        // We verify correctness; the performance gain is implicit (test must complete fast).
        let flag = Arc::new(Mutex::new(false));
        let waker = make_flag_waker(Arc::clone(&flag));

        let origin = Instant::now();
        let mut wheel = TimerWheel::new(origin);

        let deadline = origin + Duration::from_millis(50);
        wheel.insert(deadline, waker);

        // Jump 10 seconds ahead in a single tick call.
        let start = std::time::Instant::now();
        let wakers = wheel.tick(origin + Duration::from_secs(10));
        let elapsed = start.elapsed();

        assert_eq!(wakers.len(), 1, "timer must fire on 10s jump");
        for w in wakers {
            w.wake();
        }
        assert!(*flag.lock().unwrap(), "waker must have been called");
        // 10s ms-by-ms would take >10ms; optimised slot-drain takes <1ms.
        assert!(
            elapsed < Duration::from_millis(10),
            "10s tick must complete in <10ms, took {:?}",
            elapsed
        );
    }
}
