//! Global metrics registry — register at init, freeze, iterate for export.

use std::sync::{Mutex, OnceLock};

/// A named metric entry in the registry.
pub struct MetricEntry {
    pub name: &'static str,
    pub help: &'static str,
    pub kind: MetricKind,
    /// Opaque reader function — returns formatted value string for export.
    reader: Box<dyn Fn() -> MetricSnapshot + Send + Sync>,
}

/// Type tag for a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

/// A point-in-time reading of a metric.
#[derive(Debug, Clone)]
pub enum MetricSnapshot {
    Counter(u64),
    Gauge(i64),
    Histogram {
        buckets: Vec<(f64, u64)>,
        count: u64,
        sum: f64,
    },
}

/// Global metrics registry. Metrics are registered during init, then frozen.
pub struct MetricsRegistry {
    entries: Mutex<Vec<MetricEntry>>,
    frozen: std::sync::atomic::AtomicBool,
}

impl MetricsRegistry {
    const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            frozen: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Register a counter. Panics if registry is frozen.
    pub fn register_counter(&self, counter: &'static super::counter::Counter) {
        self.assert_not_frozen();
        let mut entries = self.entries.lock().unwrap();
        entries.push(MetricEntry {
            name: counter.name(),
            help: counter.help(),
            kind: MetricKind::Counter,
            reader: Box::new(move || MetricSnapshot::Counter(counter.get())),
        });
    }

    /// Register a gauge. Panics if registry is frozen.
    pub fn register_gauge(&self, gauge: &'static super::gauge::Gauge) {
        self.assert_not_frozen();
        let mut entries = self.entries.lock().unwrap();
        entries.push(MetricEntry {
            name: gauge.name(),
            help: gauge.help(),
            kind: MetricKind::Gauge,
            reader: Box::new(move || MetricSnapshot::Gauge(gauge.get())),
        });
    }

    /// Register a histogram. Panics if registry is frozen.
    pub fn register_histogram(&self, hist: &'static super::histogram::Histogram) {
        self.assert_not_frozen();
        let mut entries = self.entries.lock().unwrap();
        entries.push(MetricEntry {
            name: hist.name(),
            help: hist.help(),
            kind: MetricKind::Histogram,
            reader: Box::new(move || MetricSnapshot::Histogram {
                buckets: hist.snapshot(),
                count: hist.count(),
                sum: hist.sum(),
            }),
        });
    }

    /// Freeze the registry — no more registrations allowed.
    pub fn freeze(&self) {
        self.frozen
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether the registry is frozen.
    pub fn is_frozen(&self) -> bool {
        self.frozen.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Iterate over all registered metrics and collect snapshots.
    pub fn collect(&self) -> Vec<(&'static str, &'static str, MetricKind, MetricSnapshot)> {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .map(|e| (e.name, e.help, e.kind, (e.reader)()))
            .collect()
    }

    fn assert_not_frozen(&self) {
        if self.is_frozen() {
            panic!("MetricsRegistry is frozen — cannot register new metrics after init");
        }
    }
}

/// The global metrics registry singleton.
static GLOBAL_REGISTRY: OnceLock<MetricsRegistry> = OnceLock::new();

/// Get the global metrics registry.
pub fn global_registry() -> &'static MetricsRegistry {
    GLOBAL_REGISTRY.get_or_init(MetricsRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::counter::Counter;
    use crate::metrics::gauge::Gauge;

    static TEST_COUNTER: Counter = Counter::new("test_counter", "a test counter");
    static TEST_GAUGE: Gauge = Gauge::new("test_gauge", "a test gauge");

    #[test]
    fn registry_register_and_collect() {
        let reg = MetricsRegistry::new();
        reg.register_counter(&TEST_COUNTER);
        reg.register_gauge(&TEST_GAUGE);

        TEST_COUNTER.inc_by(5);
        TEST_GAUGE.set(42);

        let snapshots = reg.collect();
        assert_eq!(snapshots.len(), 2);

        match &snapshots[0].3 {
            MetricSnapshot::Counter(v) => assert_eq!(*v, 5),
            _ => panic!("expected counter"),
        }
        match &snapshots[1].3 {
            MetricSnapshot::Gauge(v) => assert_eq!(*v, 42),
            _ => panic!("expected gauge"),
        }
    }

    #[test]
    #[should_panic(expected = "frozen")]
    fn freeze_prevents_registration() {
        let reg = MetricsRegistry::new();
        reg.freeze();
        reg.register_counter(&TEST_COUNTER);
    }
}
