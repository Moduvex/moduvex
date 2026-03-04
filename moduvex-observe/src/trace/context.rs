//! Task-local span context for propagating trace state across `.await` boundaries.

use super::{SpanId, TraceId};
use moduvex_runtime::task_local;

task_local! {
    static SPAN_CONTEXT: SpanContext;
}

/// Holds the current trace and span stack for one async task.
#[derive(Debug, Clone)]
pub struct SpanContext {
    pub trace_id: TraceId,
    /// Stack of active span IDs (innermost last).
    pub span_stack: Vec<SpanId>,
}

impl SpanContext {
    /// Create a new context for a fresh trace.
    pub fn new() -> Self {
        Self {
            trace_id: TraceId::generate(),
            span_stack: Vec::new(),
        }
    }

    /// Create a child context inheriting the parent's trace ID.
    pub fn inherit(parent: &SpanContext) -> Self {
        Self {
            trace_id: parent.trace_id,
            span_stack: parent.span_stack.clone(),
        }
    }

    /// Push a span onto the stack.
    pub fn push_span(&mut self, span_id: SpanId) {
        self.span_stack.push(span_id);
    }

    /// Pop the innermost span.
    pub fn pop_span(&mut self) -> Option<SpanId> {
        self.span_stack.pop()
    }

    /// Get the current (innermost) span ID.
    pub fn current_span_id(&self) -> Option<SpanId> {
        self.span_stack.last().copied()
    }
}

impl Default for SpanContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a future with the given `SpanContext` attached as task-local state.
pub fn with_span_context<F: std::future::Future>(
    ctx: SpanContext,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    SPAN_CONTEXT.scope(ctx, future)
}

/// Access the current task-local SpanContext. Returns None if not in scope.
pub fn try_current_context<R>(f: impl FnOnce(&SpanContext) -> R) -> Option<R> {
    SPAN_CONTEXT.try_with(f).ok()
}

/// Access the current task-local SpanContext. Panics if not in scope.
pub fn current_context<R>(f: impl FnOnce(&SpanContext) -> R) -> R {
    SPAN_CONTEXT.with(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_context_push_pop() {
        let mut ctx = SpanContext::new();
        let s1 = SpanId::generate();
        let s2 = SpanId::generate();
        ctx.push_span(s1);
        ctx.push_span(s2);
        assert_eq!(ctx.current_span_id(), Some(s2));
        assert_eq!(ctx.pop_span(), Some(s2));
        assert_eq!(ctx.current_span_id(), Some(s1));
    }

    #[test]
    fn inherit_copies_trace_id() {
        let parent = SpanContext::new();
        let child = SpanContext::inherit(&parent);
        assert_eq!(child.trace_id, parent.trace_id);
    }

    #[test]
    fn try_current_context_returns_none_outside_scope() {
        let result = try_current_context(|ctx| ctx.trace_id);
        assert!(result.is_none());
    }
}
