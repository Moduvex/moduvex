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
//! // Structured logging
//! info!("request handled", status = 200, path = "/users");
//!
//! // Metrics
//! let counter = Counter::new("http_requests_total", "Total HTTP requests");
//! counter.inc();
//! ```

// ── Modules ──

pub mod log;
pub mod trace;
pub mod metrics;
pub mod health;
pub mod export;

// ── Re-exports: Log ──

pub use log::{Event, Level, Value};
pub use log::subscriber::{set_global_subscriber, Subscriber};
pub use log::format::{JsonFormatter, PrettyFormatter};

// ── Re-exports: Trace ──

pub use trace::{SpanId, TraceId};
pub use trace::span::{Span, SpanGuard};
pub use trace::context::SpanContext;

// ── Re-exports: Metrics ──

pub use metrics::counter::Counter;
pub use metrics::gauge::Gauge;
pub use metrics::histogram::Histogram;
pub use metrics::registry::MetricsRegistry;

// ── Re-exports: Health ──

pub use health::{HealthCheck, HealthRegistry, HealthStatus};

// ── Re-exports: Export ──

pub use export::Exporter;
pub use export::stdout::StdoutExporter;
pub use export::prometheus::PrometheusExporter;

// ── Prelude ──

pub mod prelude {
    pub use crate::{
        Counter, Event, Gauge, HealthCheck, HealthRegistry, HealthStatus,
        Histogram, Level, MetricsRegistry, Span, SpanContext, SpanGuard,
        SpanId, Subscriber, TraceId, Value,
    };
}

// ── Convenience macros ──

/// Emit a log event at the given level.
#[macro_export]
macro_rules! log_event {
    ($level:expr, $msg:expr $(, $key:ident = $val:expr)* $(,)?) => {{
        let event = $crate::Event::now($level, $msg)
            $(.field(stringify!($key), $val))*;
        $crate::log::subscriber::dispatch(&event);
    }};
}

#[macro_export]
macro_rules! error {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Error, $msg $(, $key = $val)*)
    };
}

#[macro_export]
macro_rules! warn {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Warn, $msg $(, $key = $val)*)
    };
}

#[macro_export]
macro_rules! info {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Info, $msg $(, $key = $val)*)
    };
}

#[macro_export]
macro_rules! debug {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Debug, $msg $(, $key = $val)*)
    };
}

/// Emit a TRACE-level log event.
#[macro_export]
macro_rules! trace_event {
    ($msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        $crate::log_event!($crate::Level::Trace, $msg $(, $key = $val)*)
    };
}
