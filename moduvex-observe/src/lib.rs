//! # moduvex-observe
//!
//! Observability for the Moduvex framework: structured logging, distributed
//! tracing, metrics collection, and health checks — all built on
//! `moduvex-runtime` with zero external async runtime dependencies.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use moduvex_observe::prelude::*;
//!
//! // Install the default subscriber (reads MODUVEX_LOG / MODUVEX_LOG_FORMAT).
//! moduvex_observe::init_logging();
//!
//! // Structured logging
//! info!("request handled", status = 200, path = "/users");
//!
//! // Metrics
//! let counter = Counter::new("http_requests_total", "Total HTTP requests");
//! counter.inc();
//! ```

// ── Modules ──

pub mod export;
pub mod health;
pub mod log;
pub mod metrics;
pub mod trace;

// ── Re-exports: Log ──

pub use log::format::{JsonFormatter, PrettyFormatter};
pub use log::subscriber::{
    set_global_subscriber, set_min_level, LogFormat, LogSubscriber, Subscriber,
};
pub use log::{Event, Level, Value};

// ── Re-exports: Trace ──

pub use trace::context::SpanContext;
pub use trace::span::{Span, SpanGuard};
pub use trace::{SpanId, TraceId};

// ── Re-exports: Metrics ──

pub use metrics::counter::Counter;
pub use metrics::gauge::Gauge;
pub use metrics::histogram::Histogram;
pub use metrics::registry::MetricsRegistry;

// ── Re-exports: Health ──

pub use health::{AsyncHealthCheck, HealthCheck, HealthRegistry, HealthStatus};

// ── Re-exports: Export ──

pub use export::prometheus::PrometheusExporter;
pub use export::stdout::StdoutExporter;
pub use export::Exporter;

// ── Prelude ──

pub mod prelude {
    pub use crate::{
        Counter, Event, Gauge, HealthCheck, HealthRegistry, HealthStatus, Histogram, Level,
        MetricsRegistry, Span, SpanContext, SpanGuard, SpanId, Subscriber, TraceId, Value,
    };
}

// ── Logging init ──────────────────────────────────────────────────────────────

/// Install the default structured-log subscriber.
///
/// Reads environment variables at call time:
/// - `MODUVEX_LOG` — minimum level (`trace`, `debug`, `info`, `warn`, `error`).
///   Defaults to `info`.
/// - `MODUVEX_LOG_FORMAT` — output format (`json` for JSON lines, otherwise
///   human-readable pretty format).
///
/// Also sets the global min-level atomic so that the `log_event!` macro can
/// short-circuit before allocating an `Event` for filtered-out events.
///
/// Calling this more than once is safe: subsequent calls are silently ignored
/// because `OnceLock` only accepts the first value.
pub fn init_logging() {
    let sub = LogSubscriber::from_env();
    // Raise the global filter to match the subscriber's min level, enabling
    // zero-cost filtering in the macros.
    set_min_level(sub.min_level);
    // Ignore the error — it just means a subscriber was already installed.
    let _ = set_global_subscriber(sub);
}

// ── Convenience macros ────────────────────────────────────────────────────────

/// Emit a log event at an explicit level.
///
/// The level check against the global minimum happens **before** any `Event`
/// allocation, making filtered-out events truly zero-cost.
#[macro_export]
macro_rules! log_event {
    ($level:expr, $msg:expr $(, $key:ident = $val:expr)* $(,)?) => {{
        // Zero-cost gate: compare numeric repr of levels.
        if ($level as u8) >= $crate::log::subscriber::min_level() as u8 {
            let event = $crate::Event::now($level, $msg)
                $(.field(stringify!($key), $val))*;
            $crate::log::subscriber::dispatch(&event);
        }
    }};
}

/// Emit a TRACE-level log event.
#[macro_export]
macro_rules! trace {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Trace, $msg $(, $key = $val)*)
    };
}

/// Emit a DEBUG-level log event.
#[macro_export]
macro_rules! debug {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Debug, $msg $(, $key = $val)*)
    };
}

/// Emit an INFO-level log event.
#[macro_export]
macro_rules! info {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Info, $msg $(, $key = $val)*)
    };
}

/// Emit a WARN-level log event.
#[macro_export]
macro_rules! warn {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Warn, $msg $(, $key = $val)*)
    };
}

/// Emit an ERROR-level log event.
#[macro_export]
macro_rules! error {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Error, $msg $(, $key = $val)*)
    };
}

/// Emit a TRACE-level log event (alias kept for backward compatibility).
#[macro_export]
macro_rules! trace_event {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Trace, $msg $(, $key = $val)*)
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::log::subscriber::{min_level, set_min_level, LogFormat, LogSubscriber};
    use crate::log::{Event, Level};

    // Helper: run a closure with a temporarily overridden min level, then
    // restore the previous value.  This avoids cross-test pollution.
    fn with_level<F: FnOnce()>(level: Level, f: F) {
        let prev = min_level();
        set_min_level(level);
        f();
        set_min_level(prev);
    }

    #[test]
    fn log_event_macro_passes_at_or_above_min_level() {
        // The macro itself calls dispatch() which is a no-op without a subscriber.
        // We just verify it compiles and doesn't panic.
        with_level(Level::Debug, || {
            crate::log_event!(Level::Debug, "debug msg");
            crate::log_event!(Level::Info, "info msg");
        });
    }

    #[test]
    fn trace_macro_compiles() {
        with_level(Level::Trace, || {
            crate::trace!("trace message");
            crate::trace!("trace with field", key = "value");
        });
    }

    #[test]
    fn debug_macro_compiles() {
        with_level(Level::Debug, || {
            crate::debug!("debug message");
            crate::debug!("debug with field", count = 42_i32);
        });
    }

    #[test]
    fn info_macro_compiles() {
        with_level(Level::Info, || {
            crate::info!("info message");
            crate::info!("info with fields", status = 200_i32, path = "/health");
        });
    }

    #[test]
    fn warn_macro_compiles() {
        with_level(Level::Warn, || {
            crate::warn!("warn message");
        });
    }

    #[test]
    fn error_macro_compiles() {
        with_level(Level::Error, || {
            crate::error!("error message");
            crate::error!("error with field", code = 500_i32);
        });
    }

    #[test]
    fn trace_event_alias_compiles() {
        with_level(Level::Trace, || {
            crate::trace_event!("trace alias");
        });
    }

    #[test]
    fn log_event_macro_filtered_when_below_min() {
        // Set min to Error — Debug and Info should be no-ops.
        with_level(Level::Error, || {
            // These calls should be filtered without panic.
            crate::log_event!(Level::Debug, "should be filtered");
            crate::log_event!(Level::Info, "should be filtered");
            crate::log_event!(Level::Warn, "should be filtered");
            // This should pass the filter (but dispatch is a no-op without subscriber).
            crate::log_event!(Level::Error, "should pass filter");
        });
    }

    #[test]
    fn log_subscriber_new_fields_accessible() {
        let sub = LogSubscriber::new(Level::Debug, LogFormat::Json);
        assert_eq!(sub.min_level, Level::Debug);
        assert_eq!(sub.format, LogFormat::Json);
    }

    #[test]
    fn init_logging_is_idempotent() {
        // Second call should not panic even though OnceLock is already set.
        // (In practice only the first call succeeds; subsequent ones are ignored.)
        crate::init_logging();
        crate::init_logging();
    }

    #[test]
    fn json_format_output_is_valid_structure() {
        let event = Event::now(Level::Info, "test")
            .field("key", "val")
            .field("n", 42_i32);
        let mut buf = Vec::new();
        crate::JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        // Must start with `{` and end with `}\n`.
        assert!(s.starts_with('{'));
        assert!(s.trim_end().ends_with('}'));
        assert!(s.contains("\"level\":\"INFO\""));
        assert!(s.contains("\"msg\":\"test\""));
        assert!(s.contains("\"key\":\"val\""));
        assert!(s.contains("\"n\":42"));
    }

    #[test]
    fn set_min_level_affects_macro_gate() {
        // With Error level, the u8 comparison in log_event! skips lower levels.
        with_level(Level::Error, || {
            assert_eq!(min_level(), Level::Error);
            // Trace (0) < Error (4) — filtered.
            assert!((Level::Trace as u8) < (min_level() as u8));
        });
    }
}
