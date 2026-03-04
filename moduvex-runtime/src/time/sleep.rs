//! `Sleep` future — resolves after a given `Duration`.
//!
//! On first poll the deadline is registered with the thread-local timer wheel.
//! The wheel fires the stored waker when the deadline passes, causing the
//! executor to re-poll this future, which then returns `Ready(())`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use super::{with_timer_wheel, TimerId};

/// Future that completes after `duration` has elapsed.
///
/// Created by [`sleep`]. Implements `Future<Output = ()>`.
pub struct Sleep {
    /// Absolute deadline computed from the creation time.
    deadline: Instant,
    /// Timer wheel entry, set on first poll and cleared on completion.
    timer_id: Option<TimerId>,
}

impl Sleep {
    /// Create a `Sleep` that resolves after `duration`.
    pub(crate) fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            timer_id: None,
        }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let now = Instant::now();

        // Already past deadline — return immediately.
        if now >= self.deadline {
            // Cancel any stale registration (shouldn't normally exist here).
            if let Some(id) = self.timer_id.take() {
                with_timer_wheel(|w| {
                    w.cancel(id);
                });
            }
            return Poll::Ready(());
        }

        // Register (or re-register) with the timer wheel.
        // We always re-register on each poll to keep the waker fresh (the
        // executor may have cloned a new waker since the last poll).
        if let Some(old_id) = self.timer_id.take() {
            with_timer_wheel(|w| {
                w.cancel(old_id);
            });
        }
        let id = with_timer_wheel(|w| w.insert(self.deadline, cx.waker().clone()));
        self.timer_id = Some(id);

        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        // Cancel the timer if the future is dropped before completing.
        if let Some(id) = self.timer_id.take() {
            with_timer_wheel(|w| {
                w.cancel(id);
            });
        }
    }
}

/// Returns a future that resolves after `duration` has elapsed.
///
/// # Example
/// ```no_run
/// use moduvex_runtime::time::sleep;
/// use std::time::Duration;
///
/// moduvex_runtime::block_on(async {
///     sleep(Duration::from_millis(100)).await;
///     println!("100 ms elapsed");
/// });
/// ```
pub fn sleep(duration: Duration) -> Sleep {
    Sleep::new(Instant::now() + duration)
}

/// Returns a future that resolves at the given absolute `deadline`.
pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep::new(deadline)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::block_on_with_spawn;
    use std::time::Duration;

    #[test]
    fn sleep_zero_completes_immediately() {
        block_on_with_spawn(async {
            let before = Instant::now();
            sleep(Duration::ZERO).await;
            // Should complete nearly instantly.
            assert!(before.elapsed() < Duration::from_millis(50));
        });
    }

    #[test]
    fn sleep_100ms_completes_within_bounds() {
        block_on_with_spawn(async {
            let before = Instant::now();
            sleep(Duration::from_millis(100)).await;
            let elapsed = before.elapsed();
            assert!(
                elapsed >= Duration::from_millis(95),
                "sleep resolved too early: {:?}",
                elapsed
            );
            assert!(
                elapsed < Duration::from_millis(500),
                "sleep took too long: {:?}",
                elapsed
            );
        });
    }

    #[test]
    fn sleep_drop_before_completion_does_not_panic() {
        block_on_with_spawn(async {
            // Create but immediately drop the sleep future.
            let s = sleep(Duration::from_millis(1000));
            drop(s); // Must not panic or leak.
        });
    }
}
