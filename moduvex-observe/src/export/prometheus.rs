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

    /// Render all metrics to a `String` for HTTP `/metrics` handlers.
    ///
    /// This is the primary method for integrating with HTTP servers — call it
    /// in your `/metrics` route handler to return the Prometheus text format.
    pub fn render_to_string(
        metrics: &[(&str, &str, MetricKind, MetricSnapshot)],
    ) -> std::io::Result<String> {
        let buf = Self::render_to_vec(metrics)?;
        // Prometheus format is guaranteed ASCII-subset UTF-8.
        String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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
    fn render_to_string_returns_valid_utf8() {
        let metrics = vec![(
            "http_requests_total",
            "Total HTTP requests",
            MetricKind::Counter,
            MetricSnapshot::Counter(7),
        )];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.contains("http_requests_total 7"));
        assert!(s.is_ascii());
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

    #[test]
    fn render_gauge() {
        let metrics = vec![(
            "active_connections",
            "Current active connections",
            MetricKind::Gauge,
            MetricSnapshot::Gauge(17),
        )];
        let mut buf = Vec::new();
        PrometheusExporter::render(&metrics, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("# HELP active_connections Current active connections"));
        assert!(s.contains("# TYPE active_connections gauge"));
        assert!(s.contains("active_connections 17"));
    }

    #[test]
    fn render_gauge_negative_value() {
        let metrics = vec![(
            "temperature_celsius",
            "Temperature in celsius",
            MetricKind::Gauge,
            MetricSnapshot::Gauge(-40),
        )];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.contains("temperature_celsius -40"));
    }

    #[test]
    fn render_counter_zero_value() {
        let metrics = vec![(
            "errors_total",
            "Total errors",
            MetricKind::Counter,
            MetricSnapshot::Counter(0),
        )];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.contains("errors_total 0"));
    }

    #[test]
    fn render_multiple_metrics_produces_all() {
        let metrics = vec![
            (
                "req_total",
                "requests",
                MetricKind::Counter,
                MetricSnapshot::Counter(100),
            ),
            (
                "mem_bytes",
                "memory",
                MetricKind::Gauge,
                MetricSnapshot::Gauge(1024),
            ),
        ];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.contains("req_total 100"));
        assert!(s.contains("mem_bytes 1024"));
        // Both TYPE lines present
        assert!(s.contains("# TYPE req_total counter"));
        assert!(s.contains("# TYPE mem_bytes gauge"));
    }

    #[test]
    fn render_empty_metrics_produces_empty_output() {
        let metrics: Vec<(&str, &str, MetricKind, MetricSnapshot)> = vec![];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn render_histogram_type_line() {
        let metrics = vec![(
            "latency_seconds",
            "Latency",
            MetricKind::Histogram,
            MetricSnapshot::Histogram {
                buckets: vec![(f64::INFINITY, 5)],
                count: 5,
                sum: 2.5,
            },
        )];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.contains("# TYPE latency_seconds histogram"));
    }

    #[test]
    fn render_to_vec_produces_same_as_render_to_string() {
        let metrics = vec![(
            "test_metric",
            "test",
            MetricKind::Counter,
            MetricSnapshot::Counter(3),
        )];
        let vec_output = PrometheusExporter::render_to_vec(&metrics).unwrap();
        let str_output = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert_eq!(vec_output, str_output.as_bytes());
    }

    #[test]
    fn render_histogram_empty_buckets_still_has_sum_and_count() {
        let metrics = vec![(
            "hist_no_buckets",
            "no bucket boundaries",
            MetricKind::Histogram,
            MetricSnapshot::Histogram {
                buckets: vec![(f64::INFINITY, 0)],
                count: 0,
                sum: 0.0,
            },
        )];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.contains("hist_no_buckets_sum 0"));
        assert!(s.contains("hist_no_buckets_count 0"));
    }

    #[test]
    fn render_large_counter_value() {
        let metrics = vec![(
            "huge_total",
            "big number",
            MetricKind::Counter,
            MetricSnapshot::Counter(u64::MAX),
        )];
        let s = PrometheusExporter::render_to_string(&metrics).unwrap();
        assert!(s.contains(&u64::MAX.to_string()));
    }
}
