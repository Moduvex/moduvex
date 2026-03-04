//! Global subscriber dispatch for log events.

use super::Event;
use std::sync::OnceLock;

/// Trait for receiving structured log events.
pub trait Subscriber: Send + Sync + 'static {
    /// Called for each emitted log event.
    fn on_event(&self, event: &Event);
}

/// Global subscriber slot — set once at init.
static GLOBAL_SUBSCRIBER: OnceLock<Box<dyn Subscriber>> = OnceLock::new();

/// Install a global subscriber. Returns `Err` if already set.
pub fn set_global_subscriber(sub: impl Subscriber) -> Result<(), &'static str> {
    GLOBAL_SUBSCRIBER
        .set(Box::new(sub))
        .map_err(|_| "global subscriber already set")
}

/// Dispatch an event to the global subscriber (no-op if none set).
pub fn dispatch(event: &Event) {
    if let Some(sub) = GLOBAL_SUBSCRIBER.get() {
        sub.on_event(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Level;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingSub(Arc<AtomicUsize>);

    impl Subscriber for CountingSub {
        fn on_event(&self, _event: &Event) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn dispatch_without_subscriber_is_noop() {
        // Just ensure it doesn't panic.
        let event = Event::now(Level::Info, "test");
        dispatch(&event);
    }
}
