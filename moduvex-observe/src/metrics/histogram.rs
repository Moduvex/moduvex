//! Fixed-bucket histogram for distribution tracking.

use std::sync::atomic::{AtomicU64, Ordering};

/// A histogram with fixed bucket boundaries.
///
/// Each bucket counts observations ≤ its upper bound.
/// Also tracks total count and sum for computing averages.
pub struct Histogram {
    name: &'static str,
    help: &'static str,
    /// Upper bounds for each bucket (sorted ascending).
    bounds: &'static [f64],
    /// Bucket counts — one per bound + 1 for +Inf.
    buckets: Vec<AtomicU64>,
    /// Total count of observations.
    count: AtomicU64,
    /// Sum of all observed values (stored as bits for atomicity).
    sum_bits: AtomicU64,
}

impl Histogram {
    /// Create a histogram with the given bucket boundaries.
    /// Boundaries must be sorted ascending.
    pub fn new(name: &'static str, help: &'static str, bounds: &'static [f64]) -> Self {
        let mut buckets = Vec::with_capacity(bounds.len() + 1);
        for _ in 0..=bounds.len() {
            buckets.push(AtomicU64::new(0));
        }
        Self {
            name,
            help,
            bounds,
            buckets,
            count: AtomicU64::new(0),
            sum_bits: AtomicU64::new(0),
        }
    }

    /// Record an observation. Buckets are cumulative (Prometheus convention):
    /// every bucket with bound >= value is incremented.
    pub fn observe(&self, value: f64) {
        for (i, &bound) in self.bounds.iter().enumerate() {
            if value <= bound {
                // Increment this and all higher buckets (cumulative).
                for j in i..self.bounds.len() {
                    self.buckets[j].fetch_add(1, Ordering::Relaxed);
                }
                break;
            }
        }
        // +Inf bucket always incremented.
        self.buckets[self.bounds.len()].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        // Atomic f64 add via CAS loop on bits.
        loop {
            let old_bits = self.sum_bits.load(Ordering::Relaxed);
            let old_val = f64::from_bits(old_bits);
            let new_val = old_val + value;
            let new_bits = new_val.to_bits();
            if self
                .sum_bits
                .compare_exchange_weak(old_bits, new_bits, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    /// Total number of observations.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Sum of all observed values.
    pub fn sum(&self) -> f64 {
        f64::from_bits(self.sum_bits.load(Ordering::Relaxed))
    }

    /// Snapshot of cumulative bucket counts: `(upper_bound, count)`.
    /// The last entry is `(f64::INFINITY, total_count)`.
    pub fn snapshot(&self) -> Vec<(f64, u64)> {
        let mut out = Vec::with_capacity(self.bounds.len() + 1);
        for (i, &bound) in self.bounds.iter().enumerate() {
            out.push((bound, self.buckets[i].load(Ordering::Relaxed)));
        }
        out.push((
            f64::INFINITY,
            self.buckets[self.bounds.len()].load(Ordering::Relaxed),
        ));
        out
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn help(&self) -> &'static str {
        self.help
    }

    pub fn bounds(&self) -> &'static [f64] {
        self.bounds
    }
}

// All fields are Send + Sync (atomics, &'static str/[f64], Vec<AtomicU64>), auto-derives both.

impl std::fmt::Debug for Histogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Histogram")
            .field("name", &self.name)
            .field("count", &self.count())
            .field("sum", &self.sum())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static BOUNDS: &[f64] = &[0.01, 0.05, 0.1, 0.5, 1.0, 5.0];

    #[test]
    fn histogram_basic() {
        let h = Histogram::new("latency", "request latency", BOUNDS);
        h.observe(0.042);
        h.observe(0.8);
        h.observe(2.5);

        assert_eq!(h.count(), 3);
        assert!((h.sum() - 3.342).abs() < 1e-9);

        let snap = h.snapshot();
        // Cumulative buckets:
        // 0.042: hits buckets [0.05, 0.1, 0.5, 1.0, 5.0]
        // 0.8:   hits buckets [1.0, 5.0]
        // 2.5:   hits buckets [5.0]
        assert_eq!(snap[0].1, 0); // le=0.01: 0
        assert_eq!(snap[1].1, 1); // le=0.05: 1 (0.042)
        assert_eq!(snap[2].1, 1); // le=0.1: 1 (0.042)
        assert_eq!(snap[3].1, 1); // le=0.5: 1 (0.042)
        assert_eq!(snap[4].1, 2); // le=1.0: 2 (0.042 + 0.8)
        assert_eq!(snap[5].1, 3); // le=5.0: 3 (all)
                                  // +Inf bucket = 3 (always incremented)
        assert_eq!(snap[6].1, 3);
    }

    #[test]
    fn histogram_concurrent() {
        use std::sync::Arc;
        use std::thread;

        let h = Arc::new(Histogram::new("conc", "", &[1.0, 10.0]));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let h = Arc::clone(&h);
                thread::spawn(move || {
                    for i in 0..100 {
                        h.observe(i as f64);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(h.count(), 400);
    }
}
