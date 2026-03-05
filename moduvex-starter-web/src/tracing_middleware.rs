//! W3C Trace Context (`traceparent`) middleware for distributed tracing.
//!
//! Parses the incoming `traceparent` header (version `00`), creates or inherits
//! a [`SpanContext`], wraps the downstream handler with task-local trace state,
//! and emits a `traceparent` response header with the server-side span ID.
//!
//! # Header format (W3C Trace Context Level 1)
//! ```text
//! traceparent: {version}-{trace-id}-{parent-id}-{trace-flags}
//!              00-{32hex}-{16hex}-{2hex}
//! ```
//!
//! # Usage
//! ```ignore
//! use moduvex_starter_web::TracingMiddleware;
//!
//! HttpServer::bind("0.0.0.0:8080")
//!     .middleware(TracingMiddleware::new())
//!     .serve();
//! ```

use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use moduvex_http::middleware::{Middleware, Next};
use moduvex_http::request::Request;
use moduvex_http::response::Response;
use moduvex_observe::trace::context::{with_span_context, SpanContext};
use moduvex_observe::trace::{SpanId, TraceId};

// ── Traceparent parsing ─────────────────────────────────────────────────────

/// Parsed W3C `traceparent` header fields.
#[derive(Debug, Clone)]
struct Traceparent {
    trace_id: TraceId,
    parent_id: SpanId,
    flags: u8,
}

/// Parse a `traceparent` header value.
///
/// Expected format: `00-{32hex}-{16hex}-{2hex}`
fn parse_traceparent(value: &str) -> Option<Traceparent> {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 4 {
        return None;
    }

    // Version must be "00"
    if parts[0] != "00" {
        return None;
    }

    // Trace ID: 32 hex chars → 128 bits (two u64)
    let trace_id_str = parts[1];
    if trace_id_str.len() != 32 {
        return None;
    }
    let hi = u64::from_str_radix(&trace_id_str[..16], 16).ok()?;
    let lo = u64::from_str_radix(&trace_id_str[16..], 16).ok()?;
    // All-zero trace-id is invalid per spec.
    if hi == 0 && lo == 0 {
        return None;
    }

    // Parent ID: 16 hex chars → 64 bits
    let parent_str = parts[2];
    if parent_str.len() != 16 {
        return None;
    }
    let parent = u64::from_str_radix(parent_str, 16).ok()?;
    if parent == 0 {
        return None;
    }

    // Flags: 2 hex chars → 8 bits
    let flags_str = parts[3];
    if flags_str.len() != 2 {
        return None;
    }
    let flags = u8::from_str_radix(flags_str, 16).ok()?;

    Some(Traceparent {
        trace_id: TraceId(hi, lo),
        parent_id: SpanId(parent),
        flags,
    })
}

/// Format a `traceparent` header value.
fn format_traceparent(trace_id: &TraceId, span_id: &SpanId, flags: u8) -> String {
    format!("00-{trace_id}-{span_id}-{flags:02x}")
}

// ── TracingMiddleware ───────────────────────────────────────────────────────

/// Middleware that implements W3C Trace Context propagation.
///
/// - Extracts `traceparent` from the request (or creates a fresh trace).
/// - Generates a new [`SpanId`] for the server-side span.
/// - Wraps the downstream handler with a task-local [`SpanContext`].
/// - Adds `traceparent` to the response with the same trace ID + new span ID.
/// - Logs completed span at TRACE level.
pub struct TracingMiddleware;

impl TracingMiddleware {
    /// Create a new `TracingMiddleware` instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TracingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for TracingMiddleware {
    fn handle(
        &self,
        req: Request,
        next: Next,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        Box::pin(async move {
            let incoming = req
                .headers
                .get_str("traceparent")
                .and_then(parse_traceparent);

            let server_span_id = SpanId::generate();

            let (trace_id, parent_span_id, flags) = match &incoming {
                Some(tp) => (tp.trace_id, Some(tp.parent_id), tp.flags),
                None => (TraceId::generate(), None, 0x01), // sampled by default
            };

            // Build span context with the server span pushed.
            let mut ctx = SpanContext::new();
            ctx.trace_id = trace_id;
            if let Some(parent) = parent_span_id {
                ctx.push_span(parent);
            }
            ctx.push_span(server_span_id);

            let start_us = now_us();

            // Run downstream with trace context attached.
            let mut resp = with_span_context(ctx, next.run(req)).await;

            let duration_us = now_us().saturating_sub(start_us);

            // Add traceparent to response.
            let tp_header = format_traceparent(&trace_id, &server_span_id, flags);
            resp.headers
                .insert("traceparent", tp_header.into_bytes());

            // Log completed span.
            moduvex_observe::trace_event!(
                "span completed",
                trace_id = trace_id.to_string().as_str(),
                span_id = server_span_id.to_string().as_str(),
                duration_us = duration_us as i64
            );

            resp
        })
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use moduvex_http::middleware::dispatch;
    use moduvex_http::response::Response;
    use moduvex_http::routing::method::Method;
    use moduvex_http::routing::router::BoxHandler;
    use moduvex_http::status::StatusCode;
    use std::sync::Arc;

    fn make_mw_stack() -> (Arc<Vec<Arc<dyn Middleware>>>, Arc<BoxHandler>) {
        let handler: BoxHandler =
            Box::new(|_req| Box::pin(async { Response::new(StatusCode::OK) }));
        let mws: Arc<Vec<Arc<dyn Middleware>>> =
            Arc::new(vec![Arc::new(TracingMiddleware::new())]);
        (mws, Arc::new(handler))
    }

    #[test]
    fn parse_valid_traceparent() {
        let tp = parse_traceparent(
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        )
        .unwrap();
        assert_eq!(tp.trace_id, TraceId(0x0af7651916cd43dd, 0x8448eb211c80319c));
        assert_eq!(tp.parent_id, SpanId(0xb7ad6b7169203331));
        assert_eq!(tp.flags, 0x01);
    }

    #[test]
    fn parse_invalid_traceparent_returns_none() {
        assert!(parse_traceparent("invalid").is_none());
        assert!(parse_traceparent("01-abc-def-00").is_none()); // wrong version
        assert!(parse_traceparent("00-00000000000000000000000000000000-0000000000000001-00").is_none()); // zero trace
        assert!(parse_traceparent("00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01").is_none()); // zero parent
    }

    #[test]
    fn absent_traceparent_creates_fresh_trace() {
        let (mws, handler) = make_mw_stack();
        moduvex_runtime::block_on(async {
            let req = Request::new(Method::GET, "/test");
            let resp = dispatch(&mws, &handler, req).await;
            let tp = resp.headers.get_str("traceparent").unwrap();
            let parsed = parse_traceparent(tp).unwrap();
            // Fresh trace should have a non-zero trace ID and sampled flag.
            assert_ne!(parsed.trace_id.0, 0);
            assert_eq!(parsed.flags, 0x01);
        });
    }

    #[test]
    fn valid_traceparent_propagates_trace_id() {
        let (mws, handler) = make_mw_stack();
        moduvex_runtime::block_on(async {
            let mut req = Request::new(Method::GET, "/test");
            req.headers.insert(
                "traceparent",
                b"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_vec(),
            );
            let resp = dispatch(&mws, &handler, req).await;
            let tp = resp.headers.get_str("traceparent").unwrap();
            let parsed = parse_traceparent(tp).unwrap();
            // Same trace ID propagated.
            assert_eq!(parsed.trace_id, TraceId(0x0af7651916cd43dd, 0x8448eb211c80319c));
            // Span ID must differ from parent (server generated its own).
            assert_ne!(parsed.parent_id, SpanId(0xb7ad6b7169203331));
            assert_eq!(parsed.flags, 0x01);
        });
    }

    #[test]
    fn child_span_id_differs_from_parent() {
        let (mws, handler) = make_mw_stack();
        moduvex_runtime::block_on(async {
            let mut req = Request::new(Method::GET, "/test");
            let parent_span = "b7ad6b7169203331";
            req.headers.insert(
                "traceparent",
                format!("00-0af7651916cd43dd8448eb211c80319c-{parent_span}-01")
                    .into_bytes(),
            );
            let resp = dispatch(&mws, &handler, req).await;
            let tp = resp.headers.get_str("traceparent").unwrap();
            let parts: Vec<&str> = tp.split('-').collect();
            // Response span ID (parts[2]) must not equal parent span.
            assert_ne!(parts[2], parent_span);
        });
    }

    #[test]
    fn sampled_flag_propagation() {
        let (mws, handler) = make_mw_stack();
        moduvex_runtime::block_on(async {
            let mut req = Request::new(Method::GET, "/test");
            // Flag 00 = not sampled.
            req.headers.insert(
                "traceparent",
                b"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00".to_vec(),
            );
            let resp = dispatch(&mws, &handler, req).await;
            let tp = resp.headers.get_str("traceparent").unwrap();
            let parsed = parse_traceparent(tp).unwrap();
            assert_eq!(parsed.flags, 0x00);
        });
    }
}
