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
        let event = Event::now(Level::Info, "test event")
            .field("key", "value");
        exporter.on_event(&event);
    }
}
