//! Prometheus text format exposition.
//!
//! Produces output compatible with the Prometheus text exposition format:
//! <https://prometheus.io/docs/instrumenting/exposition_formats/>

use crate::metrics::registry::{MetricKind, MetricSnapshot};
use std::io::Write;

/// Exporter that produces Prometheus text format.
pub struct PrometheusExporter;

impl PrometheusExporter {
    /// Render all metrics to Prometheus text format.
    pub fn render(
        metrics: &[(&str, &str, MetricKind, MetricSnapshot)],
        w: &mut dyn Write,
    ) -> std::io::Result<()> {
        for (name, help, kind, snapshot) in metrics {
            // TYPE and HELP lines
            writeln!(w, "# HELP {name} {help}")?;
            let type_str = match kind {
                MetricKind::Counter => "counter",
                MetricKind::Gauge => "gauge",
                MetricKind::Histogram => "histogram",
            };
            writeln!(w, "# TYPE {name} {type_str}")?;

            match snapshot {
                MetricSnapshot::Counter(v) => {
                    writeln!(w, "{name} {v}")?;
                }
                MetricSnapshot::Gauge(v) => {
                    writeln!(w, "{name} {v}")?;
                }
                MetricSnapshot::Histogram {
                    buckets,
                    count,
                    sum,
                } => {
                    for (le, cnt) in buckets {
                        if le.is_infinite() {
                            writeln!(w, "{name}_bucket{{le=\"+Inf\"}} {cnt}")?;
                        } else {
                            writeln!(w, "{name}_bucket{{le=\"{le}\"}} {cnt}")?;
                        }
                    }
                    writeln!(w, "{name}_sum {sum}")?;
                    writeln!(w, "{name}_count {count}")?;
                }
            }
        }
        Ok(())
    }

    /// Convenience: render to a `Vec<u8>` for use in HTTP responses, tests, etc.
    pub fn render_to_vec(
        metrics: &[(&str, &str, MetricKind, MetricSnapshot)],
    ) -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        Self::render(metrics, &mut buf)?;
        Ok(buf)
    }
}

impl super::Exporter for PrometheusExporter {
    fn export_metrics(
        &self,
        metrics: &[(&str, &str, MetricKind, MetricSnapshot)],
    ) -> std::io::Result<()> {
        let mut buf = Vec::new();
        Self::render(metrics, &mut buf)?;
        // Write the complete buffer to stdout in one lock to avoid interleaving.
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        lock.write_all(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_counter() {
        let metrics = vec![(
            "http_requests_total",
            "Total HTTP requests",
            MetricKind::Counter,
            MetricSnapshot::Counter(42),
        )];
        let mut buf = Vec::new();
        PrometheusExporter::render(&metrics, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("# HELP http_requests_total Total HTTP requests"));
        assert!(s.contains("# TYPE http_requests_total counter"));
        assert!(s.contains("http_requests_total 42"));
    }

    #[test]
    fn render_histogram() {
        let metrics = vec![(
            "latency",
            "Request latency",
            MetricKind::Histogram,
            MetricSnapshot::Histogram {
                buckets: vec![(0.1, 5), (0.5, 8), (f64::INFINITY, 10)],
                count: 10,
                sum: 3.5,
            },
        )];
        let mut buf = Vec::new();
        PrometheusExporter::render(&metrics, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("latency_bucket{le=\"0.1\"} 5"));
        assert!(s.contains("latency_bucket{le=\"+Inf\"} 10"));
        assert!(s.contains("latency_sum 3.5"));
        assert!(s.contains("latency_count 10"));
    }
}
