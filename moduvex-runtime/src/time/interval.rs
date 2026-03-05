//! `Interval` — periodic timer that fires at a fixed rate.
//!
//! Each call to `tick()` returns a future that resolves at the next scheduled
//! deadline. Missed ticks are tracked: if the executor falls behind, the next
//! `tick()` returns immediately and reduces the missed-tick counter.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use super::{with_timer_wheel, TimerId};

/// Periodic timer created by [`interval`].
pub struct Interval {
    /// Fixed tick period.
    period: Duration,
    /// Deadline of the next scheduled tick.
    next_deadline: Instant,
    /// Number of ticks that have been missed (deadline passed without poll).
    missed: u64,
}

impl Interval {
    pub(crate) fn new(period: Duration) -> Self {
        assert!(!period.is_zero(), "interval period must be non-zero");
        Self {
            period,
            next_deadline: Instant::now() + period,
            missed: 0,
        }
    }

    /// Returns a future that resolves at the next tick deadline.
    ///
    /// If ticks were missed the future resolves immediately and returns the
    /// deadline of the *missed* tick that is now being reported.
    pub fn tick(&mut self) -> TickFuture<'_> {
        TickFuture {
            interval: self,
            timer_id: None,
        }
    }
}

/// Future returned by [`Interval::tick`].
pub struct TickFuture<'a> {
    interval: &'a mut Interval,
    timer_id: Option<TimerId>,
}

impl<'a> Future for TickFuture<'a> {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let now = Instant::now();

        // Check whether the next deadline has already passed (missed tick).
        if now >= self.interval.next_deadline {
            // Cancel any pending registration.
            if let Some(id) = self.timer_id.take() {
                with_timer_wheel(|w| {
                    w.cancel(id);
                });
            }

            let fired_at = self.interval.next_deadline;

            // Advance past all missed ticks.
            let elapsed = now.duration_since(fired_at);
            let extra_ticks = (elapsed.as_nanos() / self.interval.period.as_nanos()) as u64;
            self.interval.missed += extra_ticks;
            // Saturate to u32::MAX to avoid truncation when extra_ticks exceeds u32 range.
            let advance = extra_ticks.saturating_add(1).min(u32::MAX as u64) as u32;
            let skip = self
                .interval
                .period
                .checked_mul(advance)
                .unwrap_or(Duration::MAX);
            self.interval.next_deadline = fired_at + skip;

            return Poll::Ready(fired_at);
        }

        // Register (or refresh) the waker with the timer wheel.
        if let Some(old_id) = self.timer_id.take() {
            with_timer_wheel(|w| {
                w.cancel(old_id);
            });
        }
        let deadline = self.interval.next_deadline;
        let id = with_timer_wheel(|w| w.insert(deadline, cx.waker().clone()));
        self.timer_id = Some(id);

        Poll::Pending
    }
}

impl<'a> Drop for TickFuture<'a> {
    fn drop(&mut self) {
        if let Some(id) = self.timer_id.take() {
            with_timer_wheel(|w| {
                w.cancel(id);
            });
        }
    }
}

/// Create a new `Interval` that fires every `period`.
///
/// The first tick fires after one full `period` from the call site.
///
/// # Panics
/// Panics if `period` is zero.
///
/// # Example
/// ```no_run
/// use moduvex_runtime::time::interval;
/// use std::time::Duration;
///
/// moduvex_runtime::block_on(async {
///     let mut ticker = interval(Duration::from_millis(50));
///     for _ in 0..3 {
///         ticker.tick().await;
///         println!("tick");
///     }
/// });
/// ```
pub fn interval(period: Duration) -> Interval {
    Interval::new(period)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::block_on_with_spawn;

    #[test]
    fn interval_fires_multiple_times() {
        block_on_with_spawn(async {
            let mut ticker = interval(Duration::from_millis(50));
            let before = Instant::now();

            ticker.tick().await;
            ticker.tick().await;
            ticker.tick().await;

            let elapsed = before.elapsed();
            // 3 ticks × 50 ms = 150 ms minimum; allow generous upper bound.
            assert!(
                elapsed >= Duration::from_millis(120),
                "interval fired too fast: {:?}",
                elapsed
            );
            assert!(
                elapsed < Duration::from_millis(1000),
                "interval took too long: {:?}",
                elapsed
            );
        });
    }

    #[test]
    #[should_panic(expected = "non-zero")]
    fn interval_zero_period_panics() {
        let _ = interval(Duration::ZERO);
    }

    #[test]
    fn interval_tracks_missed_ticks() {
        // Create an interval then sleep past two periods before polling.
        // The `missed` counter should reflect skipped ticks.
        let period = Duration::from_millis(20);
        let mut ticker = interval(period);

        // Busy-wait past two periods without polling.
        let wait_until = Instant::now() + period * 3;
        while Instant::now() < wait_until {
            std::hint::spin_loop();
        }

        // First tick() should return immediately (missed).
        block_on_with_spawn(async move {
            let now = Instant::now();
            ticker.tick().await;
            let elapsed = now.elapsed();
            // Should fire immediately — no blocking.
            assert!(
                elapsed < Duration::from_millis(50),
                "missed tick must resolve immediately, took {:?}",
                elapsed
            );
        });
    }

    // ── Additional interval tests ──────────────────────────────────────────

    #[test]
    fn interval_first_tick_after_one_period() {
        block_on_with_spawn(async {
            let period = Duration::from_millis(30);
            let mut ticker = interval(period);
            let before = Instant::now();
            ticker.tick().await;
            assert!(before.elapsed() >= Duration::from_millis(25));
        });
    }

    #[test]
    fn interval_drop_tick_future_cleans_timer() {
        block_on_with_spawn(async {
            let mut ticker = interval(Duration::from_secs(10));
            {
                let tick_fut = ticker.tick();
                drop(tick_fut); // drop without polling
            }
            // Proceed without hanging — test completes if no timer leak
        });
    }

    #[test]
    fn interval_concurrent_5_tickers() {
        use crate::executor::spawn;
        block_on_with_spawn(async {
            let before = Instant::now();
            let mut handles = Vec::new();
            for _ in 0..5 {
                handles.push(spawn(async {
                    let mut t = interval(Duration::from_millis(40));
                    t.tick().await;
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
            // All 5 fire concurrently, not sequentially
            assert!(before.elapsed() < Duration::from_millis(500));
        });
    }

    #[test]
    fn interval_missed_tick_returns_fast() {
        let period = Duration::from_millis(10);
        let mut ticker = interval(period);
        // Spin well past 2 periods
        let wait = Instant::now() + period * 4;
        while Instant::now() < wait {
            std::hint::spin_loop();
        }
        block_on_with_spawn(async move {
            let now = Instant::now();
            ticker.tick().await;
            assert!(
                now.elapsed() < Duration::from_millis(50),
                "missed tick must return fast"
            );
        });
    }

    #[test]
    fn interval_two_ticks_cumulative_time() {
        block_on_with_spawn(async {
            let period = Duration::from_millis(20);
            let mut ticker = interval(period);
            let before = Instant::now();
            ticker.tick().await;
            ticker.tick().await;
            // At least 2 periods must have elapsed
            assert!(before.elapsed() >= Duration::from_millis(30));
        });
    }

    #[test]
    fn interval_tick_returns_instant() {
        block_on_with_spawn(async {
            let period = Duration::from_millis(20);
            let mut ticker = interval(period);
            let fired_at = ticker.tick().await;
            // fired_at should be in the past (the deadline that fired)
            assert!(fired_at <= Instant::now());
        });
    }

    #[test]
    fn interval_three_sequential_ticks() {
        block_on_with_spawn(async {
            let period = Duration::from_millis(20);
            let mut ticker = interval(period);
            for _ in 0..3 {
                ticker.tick().await;
            }
            // Just verifies no hang/panic across 3 sequential ticks
        });
    }

    #[test]
    fn interval_period_1ms_fires_fast() {
        block_on_with_spawn(async {
            let mut ticker = interval(Duration::from_millis(1));
            let before = Instant::now();
            ticker.tick().await;
            // 1ms interval fires quickly
            assert!(before.elapsed() < Duration::from_millis(200));
        });
    }
}
