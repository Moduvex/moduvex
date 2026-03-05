//! Log formatters: JSON and human-readable pretty format.

use super::{Event, Value};
use std::io::Write;

/// Formats an event as a single-line JSON object.
pub struct JsonFormatter;

impl JsonFormatter {
    pub fn format(event: &Event, w: &mut dyn Write) -> std::io::Result<()> {
        // Manual JSON to avoid serde dependency.
        write!(
            w,
            "{{\"level\":\"{}\",\"ts\":{},\"msg\":\"{}\"",
            event.level.as_str(),
            event.timestamp_us,
            escape_json(&event.message),
        )?;
        for (key, val) in &event.fields {
            write!(w, ",\"{}\":", escape_json(key))?;
            match val {
                Value::String(s) => write!(w, "\"{}\"", escape_json(s))?,
                Value::I64(n) => write!(w, "{n}")?,
                Value::U64(n) => write!(w, "{n}")?,
                Value::F64(n) => {
                    if n.is_finite() {
                        write!(w, "{n}")?;
                    } else {
                        write!(w, "null")?;
                    }
                }
                Value::Bool(b) => write!(w, "{b}")?,
            }
        }
        writeln!(w, "}}")
    }
}

/// Formats an event as a human-readable colored line.
pub struct PrettyFormatter;

impl PrettyFormatter {
    pub fn format(event: &Event, w: &mut dyn Write) -> std::io::Result<()> {
        write!(w, "{} {}", event.level, event.message)?;
        for (key, val) in &event.fields {
            write!(w, " {key}={val}")?;
        }
        writeln!(w)
    }
}

/// Minimal JSON string escaping.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Level;

    #[test]
    fn json_format_basic() {
        let event = Event::now(Level::Info, "hello")
            .field("status", 200_i32)
            .field("ok", true);
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"level\":\"INFO\""));
        assert!(s.contains("\"msg\":\"hello\""));
        assert!(s.contains("\"status\":200"));
        assert!(s.contains("\"ok\":true"));
    }

    #[test]
    fn pretty_format_basic() {
        let event = Event::now(Level::Warn, "slow query").field("duration_ms", 1500_i64);
        let mut buf = Vec::new();
        PrettyFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("WARN"));
        assert!(s.contains("slow query"));
        assert!(s.contains("duration_ms=1500"));
    }

    #[test]
    fn json_escaping() {
        let event = Event::now(Level::Info, "line1\nline2");
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("line1\\nline2"));
    }

    #[test]
    fn json_escapes_carriage_return() {
        let event = Event::now(Level::Info, "line1\rline2");
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("line1\\rline2"));
    }

    #[test]
    fn json_escapes_tab() {
        let event = Event::now(Level::Info, "col1\tcol2");
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("col1\\tcol2"));
    }

    #[test]
    fn json_escapes_backslash() {
        let event = Event::now(Level::Info, "path\\to\\file");
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("path\\\\to\\\\file"));
    }

    #[test]
    fn json_escapes_double_quote() {
        let event = Event::now(Level::Info, "say \"hello\"");
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("say \\\"hello\\\""));
    }

    #[test]
    fn json_format_all_levels() {
        for &level in &[Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error] {
            let event = Event::now(level, "msg");
            let mut buf = Vec::new();
            JsonFormatter::format(&event, &mut buf).unwrap();
            let s = String::from_utf8(buf).unwrap();
            assert!(s.contains(level.as_str()));
        }
    }

    #[test]
    fn json_format_f64_value() {
        let event = Event::now(Level::Info, "f64_test").field("ratio", 1.5_f64);
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"ratio\":1.5"));
    }

    #[test]
    fn json_format_f64_nan_is_null() {
        let event = Event::now(Level::Info, "nan_test").field("bad", f64::NAN);
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"bad\":null"));
    }

    #[test]
    fn json_format_f64_infinity_is_null() {
        let event = Event::now(Level::Info, "inf_test").field("inf", f64::INFINITY);
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"inf\":null"));
    }

    #[test]
    fn json_format_output_ends_with_newline() {
        let event = Event::now(Level::Info, "newline");
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        assert_eq!(buf.last(), Some(&b'\n'));
    }

    #[test]
    fn pretty_format_output_ends_with_newline() {
        let event = Event::now(Level::Debug, "newline");
        let mut buf = Vec::new();
        PrettyFormatter::format(&event, &mut buf).unwrap();
        assert_eq!(buf.last(), Some(&b'\n'));
    }

    #[test]
    fn pretty_format_no_fields_still_has_level_and_message() {
        let event = Event::now(Level::Error, "critical failure");
        let mut buf = Vec::new();
        PrettyFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("ERROR"));
        assert!(s.contains("critical failure"));
    }

    #[test]
    fn pretty_format_multiple_fields() {
        let event = Event::now(Level::Info, "req")
            .field("method", "POST")
            .field("path", "/api/v1")
            .field("status", 201_i32);
        let mut buf = Vec::new();
        PrettyFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("method=POST"));
        assert!(s.contains("path=/api/v1"));
        assert!(s.contains("status=201"));
    }

    #[test]
    fn json_format_u64_value() {
        let event = Event::now(Level::Info, "u64").field("count", 999_u64);
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"count\":999"));
    }

    #[test]
    fn json_format_negative_i64() {
        let event = Event::now(Level::Info, "neg").field("delta", -100_i64);
        let mut buf = Vec::new();
        JsonFormatter::format(&event, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"delta\":-100"));
    }
}
