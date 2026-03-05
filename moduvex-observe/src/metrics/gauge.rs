//! Up/down gauge backed by `AtomicI64`.

use std::sync::atomic::{AtomicI64, Ordering};

/// A gauge that can go up and down.
pub struct Gauge {
    name: &'static str,
    help: &'static str,
    value: AtomicI64,
}

impl Gauge {
    /// Create a new gauge.
    pub const fn new(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            value: AtomicI64::new(0),
        }
    }

    /// Increment by 1.
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement by 1.
    pub fn dec(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    /// Add `n` (can be negative).
    pub fn add(&self, n: i64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Set to an absolute value.
    pub fn set(&self, v: i64) {
        self.value.store(v, Ordering::Relaxed);
    }

    /// Read current value.
    pub fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn help(&self) -> &'static str {
        self.help
    }
}

// All fields are Send + Sync (AtomicI64, &'static str), so Gauge auto-derives both.

impl std::fmt::Debug for Gauge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gauge")
            .field("name", &self.name)
            .field("value", &self.get())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_basic() {
        let g = Gauge::new("connections", "active connections");
        assert_eq!(g.get(), 0);
        g.inc();
        g.inc();
        assert_eq!(g.get(), 2);
        g.dec();
        assert_eq!(g.get(), 1);
        g.set(42);
        assert_eq!(g.get(), 42);
        g.add(-10);
        assert_eq!(g.get(), 32);
    }

    #[test]
    fn gauge_starts_at_zero() {
        let g = Gauge::new("fresh_gauge", "starts at zero");
        assert_eq!(g.get(), 0);
    }

    #[test]
    fn gauge_set_negative() {
        let g = Gauge::new("temperature", "celsius reading");
        g.set(-273);
        assert_eq!(g.get(), -273);
    }

    #[test]
    fn gauge_add_negative_values() {
        let g = Gauge::new("delta", "");
        g.set(100);
        g.add(-150);
        assert_eq!(g.get(), -50);
    }

    #[test]
    fn gauge_dec_below_zero() {
        let g = Gauge::new("below_zero", "");
        g.dec();
        assert_eq!(g.get(), -1);
        g.dec();
        assert_eq!(g.get(), -2);
    }

    #[test]
    fn gauge_set_to_i64_min_max() {
        let g = Gauge::new("extremes", "");
        g.set(i64::MAX);
        assert_eq!(g.get(), i64::MAX);
        g.set(i64::MIN);
        assert_eq!(g.get(), i64::MIN);
    }

    #[test]
    fn gauge_name_and_help() {
        let g = Gauge::new("queue_depth", "current queue length");
        assert_eq!(g.name(), "queue_depth");
        assert_eq!(g.help(), "current queue length");
    }

    #[test]
    fn gauge_debug_format() {
        let g = Gauge::new("dbg_gauge", "debug");
        g.set(99);
        let s = format!("{g:?}");
        assert!(s.contains("Gauge"));
        assert!(s.contains("dbg_gauge"));
        assert!(s.contains("99"));
    }

    #[test]
    fn gauge_concurrent_increments() {
        use std::sync::Arc;
        use std::thread;

        let g = Arc::new(Gauge::new("conc_gauge", ""));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let g = Arc::clone(&g);
                thread::spawn(move || {
                    for _ in 0..1000 {
                        g.inc();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(g.get(), 4000);
    }

    #[test]
    fn gauge_concurrent_inc_dec() {
        use std::sync::Arc;
        use std::thread;

        let g = Arc::new(Gauge::new("net_gauge", ""));
        // 2 threads inc, 2 threads dec — net should be 0
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let g = Arc::clone(&g);
                thread::spawn(move || {
                    for _ in 0..500 {
                        if i % 2 == 0 {
                            g.inc();
                        } else {
                            g.dec();
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(g.get(), 0);
    }

    #[test]
    fn gauge_add_zero_is_noop() {
        let g = Gauge::new("noop_gauge", "");
        g.set(55);
        g.add(0);
        assert_eq!(g.get(), 55);
    }
}
