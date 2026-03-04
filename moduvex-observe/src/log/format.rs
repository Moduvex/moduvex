//! Log formatters: JSON and human-readable pretty format.

use super::{Event, Value};
use std::io::Write;

/// Formats an event as a single-line JSON object.
pub struct JsonFormatter;

impl JsonFormatter {
    pub fn format(event: &Event, w: &mut dyn Write) -> std::io::Result<()> {
        // Manual JSON to avoid serde dependency.
        write!(w, "{{\"level\":\"{}\",\"ts\":{},\"msg\":\"{}\"",
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
        let event = Event::now(Level::Warn, "slow query")
            .field("duration_ms", 1500_i64);
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
}
