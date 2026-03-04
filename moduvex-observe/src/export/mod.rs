//! Metric and log exporters.

pub mod prometheus;
pub mod stdout;

use crate::metrics::registry::{MetricKind, MetricSnapshot};

/// Trait for exporting collected metrics.
pub trait Exporter: Send + Sync {
    /// Export a batch of metric snapshots.
    fn export_metrics(
        &self,
        metrics: &[(&str, &str, MetricKind, MetricSnapshot)],
    ) -> std::io::Result<()>;
}
