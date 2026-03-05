//! Request ID middleware — generates a unique ID per request for log correlation.
//!
//! # ID format
//! `<timestamp_ms_hex>-<counter_hex>` — e.g. `"018f3a2c1d4b-0001"`.
//!
//! Uses a monotonic millisecond timestamp combined with a per-process atomic
//! counter. This provides uniqueness within a process and rough time ordering
//! without requiring an external UUID crate.
//!
//! # Usage
//! ```ignore
//! use moduvex_http::middleware::builtin::RequestId;
//!
//! HttpServer::bind("0.0.0.0:8080")
//!     .middleware(RequestId::new())
//!     .serve();
//! ```
//!
//! # Accessing the ID in handlers
//! ```ignore
//! use moduvex_http::extract::State;
//! use moduvex_http::middleware::builtin::request_id::RequestIdValue;
//!
//! async fn handler(State(id): State<RequestIdValue>) -> String {
//!     format!("request id: {}", id.as_str())
//! }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::middleware::{Middleware, Next};
use crate::request::Request;
use crate::response::Response;

// ── ID generation ─────────────────────────────────────────────────────────────

/// Global per-process request counter — wraps after ~4 billion requests.
static REQUEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Generate a unique request ID string.
///
/// Format: `<timestamp_ms_hex>-<seq_hex>`
///
/// - `timestamp_ms` is milliseconds since UNIX epoch (fits in ~48 bits, printed
///   as hex without leading zeros).
/// - `seq` is a 4-digit zero-padded hex sequence number (0000–ffff, then wraps).
///
/// Example: `"018f3a2c1d4b-0001"`
fn generate_id() -> String {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let seq = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{ts_ms:x}-{seq:04x}")
}

// ── RequestIdValue ────────────────────────────────────────────────────────────

/// The generated request ID — stored in request extensions and response headers.
///
/// Access via `State<RequestIdValue>` in handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestIdValue(String);

impl RequestIdValue {
    /// Create a new value from a pre-generated ID string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Return the ID string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestIdValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── RequestId middleware ───────────────────────────────────────────────────────

/// Middleware that assigns a unique `X-Request-Id` to every request.
///
/// - Reads `X-Request-Id` from the incoming request; if present and non-empty,
///   that value is reused (allows upstreams/proxies to propagate IDs).
/// - Otherwise generates a new ID.
/// - Inserts the `RequestIdValue` into `request.extensions` for handler access.
/// - Appends `X-Request-Id` to the response headers.
pub struct RequestId;

impl RequestId {
    /// Create a new `RequestId` middleware instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for RequestId {
    fn handle(&self, mut req: Request, next: Next) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        // Honour an existing X-Request-Id from the client/upstream proxy,
        // or generate a fresh one.
        let id_str = req
            .headers
            .get_str("x-request-id")
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(generate_id);

        let id = RequestIdValue::new(id_str.clone());

        // Inject into request extensions so handlers can access it via State<RequestIdValue>.
        req.extensions.insert(id);

        Box::pin(async move {
            let mut resp = next.run(req).await;
            // Append the ID to the response so clients can correlate logs.
            resp.headers.insert("x-request-id", id_str.into_bytes());
            resp
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_id_format() {
        let id = generate_id();
        // Format: "<hex>-<4-hex-digits>"
        let parts: Vec<&str> = id.splitn(2, '-').collect();
        assert_eq!(parts.len(), 2, "id should contain one dash: {id}");
        // Timestamp part should be valid hex.
        assert!(
            u64::from_str_radix(parts[0], 16).is_ok(),
            "timestamp part should be hex: {}",
            parts[0]
        );
        // Counter part should be 4 hex digits.
        assert_eq!(parts[1].len(), 4, "counter part should be 4 chars: {}", parts[1]);
        assert!(
            u32::from_str_radix(parts[1], 16).is_ok(),
            "counter part should be hex: {}",
            parts[1]
        );
    }

    #[test]
    fn generate_id_increments_counter() {
        let id1 = generate_id();
        let id2 = generate_id();
        // The sequence numbers should differ (monotonically increasing).
        let seq1 = u32::from_str_radix(id1.split('-').nth(1).unwrap(), 16).unwrap();
        let seq2 = u32::from_str_radix(id2.split('-').nth(1).unwrap(), 16).unwrap();
        assert!(seq2 > seq1, "sequence should increment: {seq1} -> {seq2}");
    }

    #[test]
    fn generate_id_unique_across_calls() {
        let ids: std::collections::HashSet<String> = (0..100).map(|_| generate_id()).collect();
        assert_eq!(ids.len(), 100, "all 100 generated IDs should be unique");
    }

    #[test]
    fn request_id_value_as_str() {
        let v = RequestIdValue::new("abc-0001");
        assert_eq!(v.as_str(), "abc-0001");
    }

    #[test]
    fn request_id_value_display() {
        let v = RequestIdValue::new("test-0042");
        assert_eq!(v.to_string(), "test-0042");
    }

    #[test]
    fn request_id_value_clone_eq() {
        let a = RequestIdValue::new("x-0000");
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn middleware_adds_id_to_response() {
        use crate::middleware::dispatch;
        use crate::request::Request;
        use crate::response::Response;
        use crate::routing::method::Method;
        use crate::routing::router::BoxHandler;
        use std::sync::Arc;

        moduvex_runtime::block_on(async {
            let handler: BoxHandler = Box::new(|_req| {
                Box::pin(async { Response::new(crate::status::StatusCode::OK) })
            });
            let handler = Arc::new(handler);
            let mws: Arc<Vec<Arc<dyn crate::middleware::Middleware>>> =
                Arc::new(vec![Arc::new(RequestId::new())]);

            let req = Request::new(Method::GET, "/test");
            let resp = dispatch(&mws, &handler, req).await;

            // Response must carry X-Request-Id header.
            let id_hdr = resp.headers.get_str("x-request-id");
            assert!(id_hdr.is_some(), "expected x-request-id header in response");
            let id = id_hdr.unwrap();
            assert!(id.contains('-'), "id should contain dash separator: {id}");
        });
    }

    #[test]
    fn middleware_propagates_existing_request_id() {
        use crate::middleware::dispatch;
        use crate::request::Request;
        use crate::response::Response;
        use crate::routing::method::Method;
        use crate::routing::router::BoxHandler;
        use std::sync::Arc;

        moduvex_runtime::block_on(async {
            let handler: BoxHandler = Box::new(|_req| {
                Box::pin(async { Response::new(crate::status::StatusCode::OK) })
            });
            let handler = Arc::new(handler);
            let mws: Arc<Vec<Arc<dyn crate::middleware::Middleware>>> =
                Arc::new(vec![Arc::new(RequestId::new())]);

            let mut req = Request::new(Method::GET, "/test");
            req.headers.insert("x-request-id", b"upstream-id-123".to_vec());

            let resp = dispatch(&mws, &handler, req).await;

            // Should echo back the upstream ID, not generate a new one.
            assert_eq!(
                resp.headers.get_str("x-request-id"),
                Some("upstream-id-123"),
                "upstream x-request-id should be propagated"
            );
        });
    }

    #[test]
    fn middleware_injects_id_into_extensions() {
        use crate::request::Request;
        use crate::response::Response;
        use crate::routing::method::Method;
        use crate::routing::router::BoxHandler;
        use std::sync::Arc;

        moduvex_runtime::block_on(async {
            // Handler that reads RequestIdValue from extensions.
            let handler: BoxHandler = Box::new(|req: Request| {
                Box::pin(async move {
                    let id = req.extensions.get::<RequestIdValue>().cloned();
                    let mut resp = Response::new(crate::status::StatusCode::OK);
                    if let Some(v) = id {
                        resp.headers.insert("x-got-id", v.as_str().as_bytes().to_vec());
                    }
                    resp
                })
            });
            let handler = Arc::new(handler);
            let mws: Arc<Vec<Arc<dyn crate::middleware::Middleware>>> =
                Arc::new(vec![Arc::new(RequestId::new())]);

            let req = Request::new(Method::GET, "/");
            let resp = crate::middleware::dispatch(&mws, &handler, req).await;

            // Both x-got-id (from extension) and x-request-id (from middleware) should match.
            let from_ext = resp.headers.get_str("x-got-id").unwrap_or("");
            let from_hdr = resp.headers.get_str("x-request-id").unwrap_or("");
            assert!(!from_ext.is_empty(), "extension id should be set");
            assert_eq!(from_ext, from_hdr, "extension id should match response header");
        });
    }
}
