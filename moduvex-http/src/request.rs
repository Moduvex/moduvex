//! HTTP request type and associated metadata.
//!
//! `Request` owns the method, URI, version, headers, body, and an extension
//! map for passing data between middleware and extractors (e.g. path params,
//! shared state). It is constructed by the connection layer after parsing.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::body::Body;
use crate::header::HeaderMap;
use crate::routing::method::Method;

// ── HTTP version ──────────────────────────────────────────────────────────────

/// HTTP protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpVersion {
    Http10,
    Http11,
}

impl HttpVersion {
    /// Wire representation: `"HTTP/1.0"` or `"HTTP/1.1"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }
}

impl std::fmt::Display for HttpVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Extensions ────────────────────────────────────────────────────────────────

/// Type-erased extension map — stores one value per type.
///
/// Used to pass data extracted by middleware to handlers without modifying
/// the `Request` struct itself (e.g. path params, app state, auth principal).
#[derive(Default, Debug)]
pub struct Extensions(HashMap<TypeId, Box<dyn Any + Send + Sync>>);

impl Extensions {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Insert a value, replacing any previous value of the same type.
    pub fn insert<T: Any + Send + Sync>(&mut self, val: T) {
        self.0.insert(TypeId::of::<T>(), Box::new(val));
    }

    /// Retrieve a shared reference to a value of type `T`.
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Retrieve a mutable reference to a value of type `T`.
    pub fn get_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
        self.0
            .get_mut(&TypeId::of::<T>())
            .and_then(|b| b.downcast_mut::<T>())
    }

    /// Remove and return a value of type `T`.
    pub fn remove<T: Any + Send + Sync>(&mut self) -> Option<T> {
        self.0
            .remove(&TypeId::of::<T>())
            .and_then(|b| b.downcast::<T>().ok())
            .map(|b| *b)
    }
}

// ── Request ───────────────────────────────────────────────────────────────────

/// An incoming HTTP request.
///
/// The connection layer constructs this after parsing the request line and
/// headers. The body field is initially set from the connection buffer.
pub struct Request {
    /// HTTP method (GET, POST, …).
    pub method: Method,
    /// Request URI path (e.g. `/users/42`).
    pub path: String,
    /// Raw query string without leading `?` (e.g. `"page=1&limit=10"`).
    pub query: Option<String>,
    /// Protocol version.
    pub version: HttpVersion,
    /// Request headers.
    pub headers: HeaderMap,
    /// Request body.
    pub body: Body,
    /// Remote peer address.
    pub peer_addr: Option<SocketAddr>,
    /// Extension map for middleware/extractor data sharing.
    pub extensions: Extensions,
}

impl Request {
    /// Construct a minimal request for handler dispatch.
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: None,
            version: HttpVersion::Http11,
            headers: HeaderMap::new(),
            body: Body::Empty,
            peer_addr: None,
            extensions: Extensions::new(),
        }
    }

    /// Full URI including query: `/path?query`.
    pub fn uri(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{}", self.path, q),
            None => self.path.clone(),
        }
    }

    /// Shorthand: get a header value as UTF-8 string.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get_str(name)
    }

    /// True if `Connection: keep-alive` semantics apply.
    ///
    /// HTTP/1.1 defaults to keep-alive; HTTP/1.0 defaults to close.
    pub fn is_keep_alive(&self) -> bool {
        if let Some(v) = self.header("connection") {
            let v = v.to_ascii_lowercase();
            if v.contains("close") {
                return false;
            }
            if v.contains("keep-alive") {
                return true;
            }
        }
        self.version == HttpVersion::Http11
    }

    /// Content-Length header value, if present and valid.
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length")
            .and_then(|v| v.trim().parse().ok())
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("query", &self.query)
            .field("version", &self.version)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_alive_http11_default() {
        let req = Request::new(Method::GET, "/");
        assert!(req.is_keep_alive(), "HTTP/1.1 should default to keep-alive");
    }

    #[test]
    fn keep_alive_connection_close() {
        let mut req = Request::new(Method::GET, "/");
        req.headers.insert("connection", b"close".to_vec());
        assert!(!req.is_keep_alive());
    }

    #[test]
    fn extensions_roundtrip() {
        let mut req = Request::new(Method::GET, "/");
        req.extensions.insert(42u32);
        assert_eq!(req.extensions.get::<u32>(), Some(&42u32));
        let removed = req.extensions.remove::<u32>();
        assert_eq!(removed, Some(42u32));
        assert!(req.extensions.get::<u32>().is_none());
    }

    #[test]
    fn uri_with_query() {
        let mut req = Request::new(Method::GET, "/search");
        req.query = Some("q=rust".to_string());
        assert_eq!(req.uri(), "/search?q=rust");
    }
}
