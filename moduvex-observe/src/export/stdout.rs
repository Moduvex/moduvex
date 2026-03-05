//! Stdout exporter: prints log events and metric snapshots.

use crate::log::format::{JsonFormatter, PrettyFormatter};
use crate::log::subscriber::Subscriber;
use crate::log::Event;

/// Output format for the stdout exporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdoutFormat {
    Pretty,
    Json,
}

/// Subscriber that writes log events to stdout.
pub struct StdoutExporter {
    format: StdoutFormat,
}

impl StdoutExporter {
    pub fn new(format: StdoutFormat) -> Self {
        Self { format }
    }

    pub fn pretty() -> Self {
        Self::new(StdoutFormat::Pretty)
    }

    pub fn json() -> Self {
        Self::new(StdoutFormat::Json)
    }
}

impl Subscriber for StdoutExporter {
    fn on_event(&self, event: &Event) {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let _ = match self.format {
            StdoutFormat::Pretty => PrettyFormatter::format(event, &mut lock),
            StdoutFormat::Json => JsonFormatter::format(event, &mut lock),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Level;

    #[test]
    fn stdout_exporter_does_not_panic() {
        let exporter = StdoutExporter::json();
        let event = Event::now(Level::Info, "test event").field("key", "value");
        exporter.on_event(&event);
    }

    #[test]
    fn stdout_exporter_pretty_does_not_panic() {
        let exporter = StdoutExporter::pretty();
        let event = Event::now(Level::Warn, "pretty test").field("x", 42_i32);
        exporter.on_event(&event);
    }

    #[test]
    fn stdout_format_variants_are_distinct() {
        assert_ne!(StdoutFormat::Pretty, StdoutFormat::Json);
        assert_eq!(StdoutFormat::Json, StdoutFormat::Json);
    }

    #[test]
    fn stdout_exporter_new_pretty() {
        let exporter = StdoutExporter::new(StdoutFormat::Pretty);
        let event = Event::now(Level::Debug, "new pretty");
        exporter.on_event(&event);
    }

    #[test]
    fn stdout_exporter_new_json() {
        let exporter = StdoutExporter::new(StdoutFormat::Json);
        let event = Event::now(Level::Error, "new json");
        exporter.on_event(&event);
    }

    #[test]
    fn stdout_exporter_all_log_levels() {
        let exporter = StdoutExporter::json();
        for &level in &[Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error] {
            let event = Event::now(level, "level sweep");
            exporter.on_event(&event); // must not panic
        }
    }
}
