//! Structured logging: Level, Event, and Value types.

pub mod format;
pub mod subscriber;

use std::time::{SystemTime, UNIX_EPOCH};

/// Log severity level, ordered from most to least verbose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl Level {
    /// Short uppercase label for display.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured field value attached to an [`Event`].
#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String(s) => write!(f, "{s}"),
            Self::I64(n) => write!(f, "{n}"),
            Self::U64(n) => write!(f, "{n}"),
            Self::F64(n) => write!(f, "{n}"),
            Self::Bool(b) => write!(f, "{b}"),
        }
    }
}

// ── Into<Value> conversions ──

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}
impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Self::I64(n as i64)
    }
}
impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Self::I64(n)
    }
}
impl From<u32> for Value {
    fn from(n: u32) -> Self {
        Self::U64(n as u64)
    }
}
impl From<u64> for Value {
    fn from(n: u64) -> Self {
        Self::U64(n)
    }
}
impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::F64(n)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}
impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Self::U64(n as u64)
    }
}

/// A structured log event with level, message, timestamp, and key-value fields.
#[derive(Debug, Clone)]
pub struct Event {
    pub level: Level,
    pub message: String,
    /// Microseconds since UNIX epoch.
    pub timestamp_us: u64,
    pub fields: Vec<(&'static str, Value)>,
}

impl Event {
    /// Create an event timestamped to now.
    pub fn now(level: Level, message: &str) -> Self {
        let timestamp_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        Self {
            level,
            message: message.to_owned(),
            timestamp_us,
            fields: Vec::new(),
        }
    }

    /// Append a field (builder pattern).
    pub fn field(mut self, key: &'static str, value: impl Into<Value>) -> Self {
        self.fields.push((key, value.into()));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_ordering() {
        assert!(Level::Trace < Level::Debug);
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
    }

    #[test]
    fn event_builder() {
        let e = Event::now(Level::Info, "hello")
            .field("status", 200_i32)
            .field("path", "/users");
        assert_eq!(e.level, Level::Info);
        assert_eq!(e.message, "hello");
        assert_eq!(e.fields.len(), 2);
    }

    #[test]
    fn value_display() {
        assert_eq!(Value::String("hi".into()).to_string(), "hi");
        assert_eq!(Value::I64(-42).to_string(), "-42");
        assert_eq!(Value::U64(100).to_string(), "100");
        assert_eq!(Value::Bool(true).to_string(), "true");
    }

    #[test]
    fn level_as_str() {
        assert_eq!(Level::Trace.as_str(), "TRACE");
        assert_eq!(Level::Debug.as_str(), "DEBUG");
        assert_eq!(Level::Info.as_str(), "INFO");
        assert_eq!(Level::Warn.as_str(), "WARN");
        assert_eq!(Level::Error.as_str(), "ERROR");
    }

    #[test]
    fn level_display_matches_as_str() {
        for &lvl in &[Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error] {
            assert_eq!(lvl.to_string(), lvl.as_str());
        }
    }

    #[test]
    fn level_equality() {
        assert_eq!(Level::Info, Level::Info);
        assert_ne!(Level::Info, Level::Warn);
    }

    #[test]
    fn event_has_timestamp() {
        let e = Event::now(Level::Debug, "ts test");
        // Timestamp should be after year 2000 in microseconds
        assert!(e.timestamp_us > 946_684_800_000_000);
    }

    #[test]
    fn event_empty_fields_by_default() {
        let e = Event::now(Level::Error, "bare event");
        assert!(e.fields.is_empty());
    }

    #[test]
    fn value_f64_display() {
        assert_eq!(Value::F64(3.14).to_string(), "3.14");
    }

    #[test]
    fn value_bool_false_display() {
        assert_eq!(Value::Bool(false).to_string(), "false");
    }

    #[test]
    fn value_from_str_ref() {
        let v: Value = "hello".into();
        assert!(matches!(v, Value::String(_)));
        assert_eq!(v.to_string(), "hello");
    }

    #[test]
    fn value_from_string_owned() {
        let v: Value = String::from("owned").into();
        assert!(matches!(v, Value::String(_)));
    }

    #[test]
    fn value_from_i32() {
        let v: Value = (-5_i32).into();
        assert!(matches!(v, Value::I64(-5)));
    }

    #[test]
    fn value_from_i64() {
        let v: Value = i64::MIN.into();
        assert!(matches!(v, Value::I64(_)));
    }

    #[test]
    fn value_from_u32() {
        let v: Value = (42_u32).into();
        assert!(matches!(v, Value::U64(42)));
    }

    #[test]
    fn value_from_u64() {
        let v: Value = u64::MAX.into();
        assert!(matches!(v, Value::U64(_)));
    }

    #[test]
    fn value_from_f64() {
        let v: Value = (2.718_f64).into();
        assert!(matches!(v, Value::F64(_)));
    }

    #[test]
    fn value_from_bool() {
        let v: Value = true.into();
        assert!(matches!(v, Value::Bool(true)));
    }

    #[test]
    fn value_from_usize() {
        let v: Value = (100_usize).into();
        assert!(matches!(v, Value::U64(100)));
    }

    #[test]
    fn event_multiple_fields() {
        let e = Event::now(Level::Warn, "many fields")
            .field("a", 1_i32)
            .field("b", "two")
            .field("c", true)
            .field("d", 4.0_f64);
        assert_eq!(e.fields.len(), 4);
        assert_eq!(e.fields[0].0, "a");
        assert_eq!(e.fields[1].0, "b");
    }

    #[test]
    fn value_clone() {
        let v = Value::String("cloned".into());
        let v2 = v.clone();
        assert_eq!(v.to_string(), v2.to_string());
    }
}
