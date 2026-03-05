//! Span lifecycle with RAII guard for automatic enter/exit.

use super::{SpanId, TraceId};
use crate::log::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// A unit of work within a trace.
#[derive(Debug, Clone)]
pub struct Span {
    pub name: &'static str,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    /// Start timestamp in microseconds since UNIX epoch.
    pub start_us: u64,
    /// End timestamp (set when span closes).
    pub end_us: Option<u64>,
    pub fields: Vec<(&'static str, Value)>,
}

impl Span {
    /// Create a new root span (no parent) with a fresh TraceId.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            trace_id: TraceId::generate(),
            span_id: SpanId::generate(),
            parent_span_id: None,
            start_us: now_us(),
            end_us: None,
            fields: Vec::new(),
        }
    }

    /// Create a child span inheriting the parent's TraceId.
    pub fn child(name: &'static str, parent: &Span) -> Self {
        Self {
            name,
            trace_id: parent.trace_id,
            span_id: SpanId::generate(),
            parent_span_id: Some(parent.span_id),
            start_us: now_us(),
            end_us: None,
            fields: Vec::new(),
        }
    }

    /// Add a field to this span (builder pattern).
    pub fn with_field(mut self, key: &'static str, value: impl Into<Value>) -> Self {
        self.fields.push((key, value.into()));
        self
    }

    /// Enter this span, returning an RAII guard that records duration on drop.
    pub fn enter(&mut self) -> SpanGuard<'_> {
        SpanGuard { span: self }
    }

    /// Close the span, recording the end timestamp.
    pub fn close(&mut self) {
        if self.end_us.is_none() {
            self.end_us = Some(now_us());
        }
    }

    /// Duration in microseconds, or None if still open.
    pub fn duration_us(&self) -> Option<u64> {
        self.end_us.map(|end| end.saturating_sub(self.start_us))
    }
}

/// RAII guard — closes the span when dropped.
pub struct SpanGuard<'a> {
    span: &'a mut Span,
}

impl<'a> SpanGuard<'a> {
    /// Access the underlying span.
    pub fn span(&self) -> &Span {
        self.span
    }
}

impl Drop for SpanGuard<'_> {
    fn drop(&mut self) {
        self.span.close();
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_records_duration() {
        let mut span = Span::new("test_op");
        assert!(span.end_us.is_none());
        {
            let _guard = span.enter();
            // guard drops here
        }
        assert!(span.end_us.is_some());
        assert!(span.duration_us().unwrap() < 1_000_000); // < 1s
    }

    #[test]
    fn child_inherits_trace_id() {
        let parent = Span::new("parent");
        let child = Span::child("child", &parent);
        assert_eq!(child.trace_id, parent.trace_id);
        assert_eq!(child.parent_span_id, Some(parent.span_id));
        assert_ne!(child.span_id, parent.span_id);
    }

    #[test]
    fn span_fields() {
        let span = Span::new("request")
            .with_field("method", "GET")
            .with_field("status", 200_i32);
        assert_eq!(span.fields.len(), 2);
    }

    #[test]
    fn span_new_has_no_parent() {
        let span = Span::new("root");
        assert!(span.parent_span_id.is_none());
    }

    #[test]
    fn span_new_is_open() {
        let span = Span::new("open_span");
        assert!(span.end_us.is_none());
        assert!(span.duration_us().is_none());
    }

    #[test]
    fn span_close_sets_end_timestamp() {
        let mut span = Span::new("closeable");
        span.close();
        assert!(span.end_us.is_some());
    }

    #[test]
    fn span_close_idempotent() {
        let mut span = Span::new("idempotent");
        span.close();
        let first_end = span.end_us;
        span.close(); // second close should not update end_us
        assert_eq!(span.end_us, first_end);
    }

    #[test]
    fn span_guard_accesses_span() {
        let mut span = Span::new("guarded");
        span = span.with_field("key", "val");
        let guard = span.enter();
        assert_eq!(guard.span().name, "guarded");
        assert_eq!(guard.span().fields.len(), 1);
        drop(guard);
    }

    #[test]
    fn span_duration_is_non_negative() {
        let mut span = Span::new("timing");
        {
            let _g = span.enter();
        }
        let dur = span.duration_us().unwrap();
        // Duration should be 0 or more (saturating_sub ensures non-negative)
        assert!(dur < 1_000_000_000); // sanity: less than 1000 seconds
    }

    #[test]
    fn nested_spans_have_unique_ids() {
        let root = Span::new("root");
        let child1 = Span::child("child1", &root);
        let child2 = Span::child("child2", &root);
        assert_ne!(child1.span_id, child2.span_id);
        assert_ne!(child1.span_id, root.span_id);
    }

    #[test]
    fn deeply_nested_span_inherits_trace_id() {
        let root = Span::new("root");
        let c1 = Span::child("c1", &root);
        let c2 = Span::child("c2", &c1);
        let c3 = Span::child("c3", &c2);
        assert_eq!(c3.trace_id, root.trace_id);
    }

    #[test]
    fn span_with_multiple_fields() {
        let span = Span::new("multi_field")
            .with_field("a", "alpha")
            .with_field("b", 42_i32)
            .with_field("c", true)
            .with_field("d", 3.14_f64);
        assert_eq!(span.fields.len(), 4);
    }

    #[test]
    fn span_start_us_is_positive() {
        let span = Span::new("ts_check");
        // start_us should be a valid UNIX timestamp (after year 2000)
        assert!(span.start_us > 946_684_800_000_000); // 2000-01-01 in microseconds
    }

    #[test]
    fn span_debug_format() {
        let span = Span::new("debug_span");
        let s = format!("{span:?}");
        assert!(s.contains("Span"));
        assert!(s.contains("debug_span"));
    }
}
