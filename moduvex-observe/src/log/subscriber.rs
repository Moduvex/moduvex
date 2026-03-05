//! Global subscriber dispatch for log events.
//!
//! Provides:
//! - A global min-level filter backed by an `AtomicU8` (zero-cost when filtered out)
//! - `LogSubscriber`: a ready-to-use subscriber that writes to stderr in Pretty or JSON format
//! - `set_global_subscriber` / `dispatch` for wiring everything together

use super::{Event, Level};
use crate::log::format::{JsonFormatter, PrettyFormatter};
use std::io::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

// ── Global min-level filter ───────────────────────────────────────────────────

/// Global minimum log level. Events below this level are discarded before
/// allocating an `Event` struct (see `log_event!` macro).
///
/// Default: `Trace` — all events pass through until a subscriber or
/// `init_logging()` raises it.
static GLOBAL_MIN_LEVEL: AtomicU8 = AtomicU8::new(Level::Trace as u8);

/// Set the global minimum log level.
///
/// All events with a level strictly below `level` will be dropped in the
/// `log_event!` macro before any allocation occurs.
pub fn set_min_level(level: Level) {
    GLOBAL_MIN_LEVEL.store(level as u8, Ordering::Release);
}

/// Return the current global minimum log level.
pub fn min_level() -> Level {
    // Safety: `GLOBAL_MIN_LEVEL` is only ever written via `set_min_level`
    // which accepts a `Level` value.  The Level repr is u8 with values 0-4,
    // and `AtomicU8::new` is initialised with a valid `Level as u8`.
    let raw = GLOBAL_MIN_LEVEL.load(Ordering::Acquire);
    // Clamp to valid range as a belt-and-suspenders guard.
    match raw {
        0 => Level::Trace,
        1 => Level::Debug,
        2 => Level::Info,
        3 => Level::Warn,
        _ => Level::Error,
    }
}

// ── Subscriber trait ──────────────────────────────────────────────────────────

/// Trait for receiving structured log events.
pub trait Subscriber: Send + Sync + 'static {
    /// Called for each emitted log event that passes the min-level filter.
    fn on_event(&self, event: &Event);
}

// ── Global subscriber slot ────────────────────────────────────────────────────

/// Global subscriber slot — set once at init.
static GLOBAL_SUBSCRIBER: OnceLock<Box<dyn Subscriber>> = OnceLock::new();

/// Install a global subscriber. Returns `Err` if already set.
pub fn set_global_subscriber(sub: impl Subscriber) -> Result<(), &'static str> {
    GLOBAL_SUBSCRIBER
        .set(Box::new(sub))
        .map_err(|_| "global subscriber already set")
}

/// Dispatch an event to the global subscriber (no-op if none set).
///
/// The macro `log_event!` already checks `min_level()` before calling this,
/// so the level gate here is intentionally omitted for performance.
pub fn dispatch(event: &Event) {
    if let Some(sub) = GLOBAL_SUBSCRIBER.get() {
        sub.on_event(event);
    }
}

// ── LogFormat ─────────────────────────────────────────────────────────────────

/// Output format for `LogSubscriber`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable: `LEVEL message key=value …`
    Pretty,
    /// Machine-readable: one JSON object per line.
    Json,
}

// ── LogSubscriber ─────────────────────────────────────────────────────────────

/// A ready-to-use subscriber that writes to stderr.
///
/// Filtering is two-layered:
/// 1. The `log_event!` macro checks `GLOBAL_MIN_LEVEL` before building an `Event`.
/// 2. `LogSubscriber::on_event` re-checks its own `min_level` field, which allows
///    multiple subscribers with different per-instance thresholds if needed.
pub struct LogSubscriber {
    /// Minimum level this subscriber accepts.
    pub min_level: Level,
    /// Output format.
    pub format: LogFormat,
}

impl LogSubscriber {
    /// Create a new subscriber with explicit configuration.
    pub fn new(min_level: Level, format: LogFormat) -> Self {
        Self { min_level, format }
    }

    /// Build a subscriber from environment variables.
    ///
    /// - `MODUVEX_LOG` controls the level (e.g. `debug`, `info`, `warn`).
    ///   Defaults to `info` when absent or unrecognised.
    /// - `MODUVEX_LOG_FORMAT` controls the format (`json` → JSON, anything else → Pretty).
    pub fn from_env() -> Self {
        let level = std::env::var("MODUVEX_LOG")
            .ok()
            .and_then(|s| parse_level(&s))
            .unwrap_or(Level::Info);

        let format = match std::env::var("MODUVEX_LOG_FORMAT")
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "json" => LogFormat::Json,
            _ => LogFormat::Pretty,
        };

        Self::new(level, format)
    }
}

impl Subscriber for LogSubscriber {
    fn on_event(&self, event: &Event) {
        // Per-subscriber level gate (in addition to the global atomic check).
        if event.level < self.min_level {
            return;
        }
        let stderr = std::io::stderr();
        let mut w = stderr.lock();
        match self.format {
            LogFormat::Pretty => {
                let _ = PrettyFormatter::format(event, &mut w);
            }
            LogFormat::Json => {
                let _ = JsonFormatter::format(event, &mut w);
            }
        }
        // Ignore flush errors — stderr is best-effort.
        let _ = w.flush();
    }
}

// ── Level parsing ─────────────────────────────────────────────────────────────

/// Parse a level string (case-insensitive).  Returns `None` for unknown values.
pub fn parse_level(s: &str) -> Option<Level> {
    match s.to_ascii_lowercase().as_str() {
        "trace" => Some(Level::Trace),
        "debug" => Some(Level::Debug),
        "info" => Some(Level::Info),
        "warn" | "warning" => Some(Level::Warn),
        "error" => Some(Level::Error),
        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Level;
    use std::sync::{Arc, Mutex};

    struct CaptureSub {
        events: Arc<Mutex<Vec<Event>>>,
        min: Level,
    }

    impl Subscriber for CaptureSub {
        fn on_event(&self, event: &Event) {
            if event.level >= self.min {
                self.events.lock().unwrap().push(event.clone());
            }
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn dispatch_without_subscriber_is_noop() {
        // Just ensure it doesn't panic.
        let event = Event::now(Level::Info, "test");
        dispatch(&event);
    }

    #[test]
    fn parse_level_valid() {
        assert_eq!(parse_level("trace"), Some(Level::Trace));
        assert_eq!(parse_level("debug"), Some(Level::Debug));
        assert_eq!(parse_level("info"), Some(Level::Info));
        assert_eq!(parse_level("warn"), Some(Level::Warn));
        assert_eq!(parse_level("warning"), Some(Level::Warn));
        assert_eq!(parse_level("error"), Some(Level::Error));
    }

    #[test]
    fn parse_level_case_insensitive() {
        assert_eq!(parse_level("INFO"), Some(Level::Info));
        assert_eq!(parse_level("Debug"), Some(Level::Debug));
        assert_eq!(parse_level("WARN"), Some(Level::Warn));
    }

    #[test]
    fn parse_level_unknown_returns_none() {
        assert_eq!(parse_level("verbose"), None);
        assert_eq!(parse_level(""), None);
        assert_eq!(parse_level("3"), None);
    }

    #[test]
    fn set_min_level_and_read_back() {
        // Save original to restore after test.
        let original = min_level();

        set_min_level(Level::Warn);
        assert_eq!(min_level(), Level::Warn);

        set_min_level(Level::Trace);
        assert_eq!(min_level(), Level::Trace);

        // Restore.
        set_min_level(original);
    }

    #[test]
    fn log_subscriber_filters_below_min_level() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let cap = CaptureSub {
            events: Arc::clone(&events),
            min: Level::Warn,
        };

        // Debug event — should be filtered.
        cap.on_event(&Event::now(Level::Debug, "debug msg"));
        // Info event — should be filtered.
        cap.on_event(&Event::now(Level::Info, "info msg"));
        // Warn event — should pass.
        cap.on_event(&Event::now(Level::Warn, "warn msg"));
        // Error event — should pass.
        cap.on_event(&Event::now(Level::Error, "error msg"));

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].level, Level::Warn);
        assert_eq!(captured[1].level, Level::Error);
    }

    #[test]
    fn log_subscriber_pretty_writes_to_buffer() {
        let sub = LogSubscriber::new(Level::Trace, LogFormat::Pretty);
        // Just ensure it doesn't panic when writing to stderr.
        sub.on_event(&Event::now(Level::Info, "pretty test").field("k", "v"));
    }

    #[test]
    fn log_subscriber_json_writes_to_buffer() {
        let sub = LogSubscriber::new(Level::Trace, LogFormat::Json);
        sub.on_event(&Event::now(Level::Info, "json test").field("x", 1_i32));
    }

    #[test]
    fn log_subscriber_from_env_defaults_to_info_pretty() {
        // Remove env vars to test defaults.
        std::env::remove_var("MODUVEX_LOG");
        std::env::remove_var("MODUVEX_LOG_FORMAT");

        let sub = LogSubscriber::from_env();
        assert_eq!(sub.min_level, Level::Info);
        assert_eq!(sub.format, LogFormat::Pretty);
    }

    #[test]
    fn log_subscriber_from_env_reads_level() {
        std::env::set_var("MODUVEX_LOG", "debug");
        let sub = LogSubscriber::from_env();
        assert_eq!(sub.min_level, Level::Debug);
        std::env::remove_var("MODUVEX_LOG");
    }

    #[test]
    fn log_subscriber_from_env_reads_json_format() {
        std::env::set_var("MODUVEX_LOG_FORMAT", "json");
        let sub = LogSubscriber::from_env();
        assert_eq!(sub.format, LogFormat::Json);
        std::env::remove_var("MODUVEX_LOG_FORMAT");
    }

    #[test]
    fn log_subscriber_from_env_unknown_level_defaults_to_info() {
        std::env::set_var("MODUVEX_LOG", "verbose");
        let sub = LogSubscriber::from_env();
        assert_eq!(sub.min_level, Level::Info);
        std::env::remove_var("MODUVEX_LOG");
    }

    #[test]
    fn log_format_eq() {
        assert_eq!(LogFormat::Pretty, LogFormat::Pretty);
        assert_eq!(LogFormat::Json, LogFormat::Json);
        assert_ne!(LogFormat::Pretty, LogFormat::Json);
    }
}
