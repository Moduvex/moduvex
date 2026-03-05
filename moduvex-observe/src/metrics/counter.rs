//! Monotonic counter backed by `AtomicU64`.

use std::sync::atomic::{AtomicU64, Ordering};

/// A monotonically increasing counter.
pub struct Counter {
    name: &'static str,
    help: &'static str,
    value: AtomicU64,
}

impl Counter {
    /// Create a new counter with the given name and help text.
    pub const fn new(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            value: AtomicU64::new(0),
        }
    }

    /// Increment by 1.
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment by `n`.
    pub fn inc_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Read current value.
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn help(&self) -> &'static str {
        self.help
    }
}

// All fields are Send + Sync (AtomicU64, &'static str), so Counter auto-derives both.

impl std::fmt::Debug for Counter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Counter")
            .field("name", &self.name)
            .field("value", &self.get())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_basic() {
        let c = Counter::new("test_total", "test counter");
        assert_eq!(c.get(), 0);
        c.inc();
        assert_eq!(c.get(), 1);
        c.inc_by(10);
        assert_eq!(c.get(), 11);
    }

    #[test]
    fn counter_concurrent() {
        use std::sync::Arc;
        use std::thread;

        let c = Arc::new(Counter::new("conc", ""));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let c = Arc::clone(&c);
                thread::spawn(move || {
                    for _ in 0..1000 {
                        c.inc();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(c.get(), 4000);
    }

    #[test]
    fn counter_starts_at_zero() {
        let c = Counter::new("fresh", "fresh counter");
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn counter_inc_by_zero_is_noop() {
        let c = Counter::new("noop", "");
        c.inc_by(0);
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn counter_inc_by_large_value() {
        let c = Counter::new("large", "");
        c.inc_by(u64::MAX / 2);
        assert_eq!(c.get(), u64::MAX / 2);
    }

    #[test]
    fn counter_name_and_help() {
        let c = Counter::new("my_metric_total", "counts things");
        assert_eq!(c.name(), "my_metric_total");
        assert_eq!(c.help(), "counts things");
    }

    #[test]
    fn counter_debug_format() {
        let c = Counter::new("dbg", "debug test");
        c.inc_by(7);
        let s = format!("{c:?}");
        assert!(s.contains("Counter"));
        assert!(s.contains("dbg"));
        assert!(s.contains('7'));
    }

    #[test]
    fn counter_multiple_increments_accumulate() {
        let c = Counter::new("accum", "");
        for i in 1u64..=100 {
            c.inc_by(i);
        }
        // sum(1..=100) = 5050
        assert_eq!(c.get(), 5050);
    }

    #[test]
    fn counter_inc_and_inc_by_interleaved() {
        let c = Counter::new("mixed", "");
        c.inc();
        c.inc_by(9);
        c.inc();
        assert_eq!(c.get(), 11);
    }

    #[test]
    fn counter_concurrent_inc_by() {
        use std::sync::Arc;
        use std::thread;

        let c = Arc::new(Counter::new("conc_by", ""));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let c = Arc::clone(&c);
                thread::spawn(move || {
                    for _ in 0..500 {
                        c.inc_by(2);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // 8 threads * 500 iterations * 2 = 8000
        assert_eq!(c.get(), 8000);
    }
}
