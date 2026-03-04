//! Async timer primitives: [`sleep`], [`sleep_until`], [`interval`].
//!
//! All timers are driven by a per-thread hierarchical timer wheel integrated
//! into the executor run loop. The wheel is ticked each iteration of the loop;
//! expired timers fire their wakers, re-scheduling the waiting tasks.
//!
//! # Thread-local timer wheel
//! The `TIMER_WHEEL` thread-local holds the wheel for the current executor
//! thread. `with_timer_wheel` provides safe, borrow-scoped mutable access.

pub mod wheel;
pub mod sleep;
pub mod interval;

pub(crate) use wheel::{TimerWheel, TimerId};
pub use sleep::{sleep, sleep_until, Sleep};
pub use interval::{interval, Interval};

use std::cell::RefCell;
use std::time::Instant;

// ── Thread-local timer wheel ──────────────────────────────────────────────────

thread_local! {
    /// Per-thread timer wheel. Lazily initialised on first access.
    /// Origin is captured once at init time and stays fixed.
    static TIMER_WHEEL: RefCell<TimerWheel> =
        RefCell::new(TimerWheel::new(Instant::now()));
}

/// Mutably borrow the thread-local timer wheel for the duration of `f`.
///
/// # Panics
/// Panics on re-entrant borrow (same contract as `RefCell::borrow_mut`).
pub(crate) fn with_timer_wheel<F, R>(f: F) -> R
where
    F: FnOnce(&mut TimerWheel) -> R,
{
    TIMER_WHEEL.with(|cell| f(&mut cell.borrow_mut()))
}

/// Tick the thread-local timer wheel to `now`, returning expired wakers.
///
/// Called by the executor run loop every iteration.
pub(crate) fn tick_timer_wheel(now: Instant) -> Vec<std::task::Waker> {
    with_timer_wheel(|w| w.tick(now))
}

/// Return the nearest pending deadline from the thread-local timer wheel.
///
/// Used by the executor to compute the reactor poll timeout.
pub(crate) fn next_timer_deadline() -> Option<Instant> {
    with_timer_wheel(|w| w.next_deadline())
}
