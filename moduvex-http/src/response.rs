//! HTTP response type and `IntoResponse` trait.
//!
//! `Response` owns a status code, headers, and a body. `IntoResponse` is the
//! ergonomic conversion trait implemented for common types so handler functions
//! can return `&str`, `String`, `(StatusCode, Body)`, etc. directly.

use crate::body::Body;
use crate::header::HeaderMap;
use crate::request::Extensions;
use crate::status::StatusCode;

// ── Response ──────────────────────────────────────────────────────────────────

/// An HTTP response ready to be serialised and sent over the wire.
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Body,
    /// Extension map for out-of-band data (e.g. WebSocket upgrade callbacks).
    ///
    /// Not serialised over the wire — consumed by the connection layer before
    /// encoding.  Default is empty; middleware and extractors may populate it.
    pub extensions: Extensions,
}

impl Response {
    /// Create a response with the given status and an empty body.
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: Body::Empty,
            extensions: Extensions::new(),
        }
    }

    /// Create a response with status, headers, and body.
    pub fn with_body(status: StatusCode, body: impl Into<Body>) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: body.into(),
            extensions: Extensions::new(),
        }
    }

    /// Set a header on the response.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Set the `Content-Type` header.
    pub fn content_type(self, ct: &str) -> Self {
        self.header("content-type", ct.as_bytes().to_vec())
    }

    /// Shorthand: plain-text 200 response.
    pub fn text(body: impl Into<String>) -> Self {
        let bytes: Vec<u8> = body.into().into_bytes();
        Self::with_body(StatusCode::OK, bytes).content_type("text/plain; charset=utf-8")
    }

    /// Shorthand: JSON 200 response (body must already be serialised JSON bytes).
    pub fn json(body: impl Into<Vec<u8>>) -> Self {
        Self::with_body(StatusCode::OK, body.into()).content_type("application/json")
    }

    /// Shorthand: 404 Not Found with plain-text body.
    pub fn not_found() -> Self {
        Self::with_body(StatusCode::NOT_FOUND, "404 Not Found")
            .content_type("text/plain; charset=utf-8")
    }

    /// Shorthand: 500 Internal Server Error.
    pub fn internal_error() -> Self {
        Self::with_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            "500 Internal Server Error",
        )
        .content_type("text/plain; charset=utf-8")
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

// ── IntoResponse ──────────────────────────────────────────────────────────────

/// Convert `Self` into an HTTP [`Response`].
///
/// Implemented for common return types so handler functions can return them
/// directly without constructing a `Response` manually.
pub trait IntoResponse {
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        Response::new(self)
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> Response {
        Response::text(self)
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        Response::text(self)
    }
}

impl IntoResponse for Vec<u8> {
    fn into_response(self) -> Response {
        Response::with_body(StatusCode::OK, self).content_type("application/octet-stream")
    }
}

/// `(StatusCode, body)` tuple — set status and body together.
impl<B: Into<Body>> IntoResponse for (StatusCode, B) {
    fn into_response(self) -> Response {
        Response::with_body(self.0, self.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_response() {
        let r = Response::text("hello");
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(
            r.headers.get_str("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(r.body.into_bytes(), b"hello");
    }

    #[test]
    fn into_response_str() {
        let r = "world".into_response();
        assert_eq!(r.status, StatusCode::OK);
    }

    #[test]
    fn into_response_tuple() {
        let r = (StatusCode::CREATED, "created").into_response();
        assert_eq!(r.status, StatusCode::CREATED);
    }

    #[test]
    fn not_found_helper() {
        let r = Response::not_found();
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn internal_error_helper() {
        let r = Response::internal_error();
        assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            r.headers.get_str("content-type"),
            Some("text/plain; charset=utf-8")
        );
    }

    #[test]
    fn json_response_sets_content_type() {
        let r = Response::json(b"{}".to_vec());
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(
            r.headers.get_str("content-type"),
            Some("application/json")
        );
    }

    #[test]
    fn response_with_body_status() {
        let r = Response::with_body(StatusCode::CREATED, "resource created");
        assert_eq!(r.status, StatusCode::CREATED);
    }

    #[test]
    fn response_header_builder() {
        let r = Response::new(StatusCode::OK)
            .header("x-foo", b"bar".to_vec())
            .header("x-baz", b"qux".to_vec());
        assert_eq!(r.headers.get_str("x-foo"), Some("bar"));
        assert_eq!(r.headers.get_str("x-baz"), Some("qux"));
    }

    #[test]
    fn response_content_type_builder() {
        let r = Response::new(StatusCode::OK).content_type("text/html");
        assert_eq!(
            r.headers.get_str("content-type"),
            Some("text/html")
        );
    }

    #[test]
    fn into_response_vec_u8_sets_octet_stream() {
        let bytes: Vec<u8> = vec![0x01, 0x02, 0x03];
        let r = bytes.into_response();
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(
            r.headers.get_str("content-type"),
            Some("application/octet-stream")
        );
    }

    #[test]
    fn into_response_string() {
        let r = String::from("response body").into_response();
        assert_eq!(r.status, StatusCode::OK);
    }

    #[test]
    fn into_response_status_code() {
        let r = StatusCode::NO_CONTENT.into_response();
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
}
