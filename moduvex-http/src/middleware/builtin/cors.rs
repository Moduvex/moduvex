//! CORS middleware — Cross-Origin Resource Sharing headers.

use std::future::Future;
use std::pin::Pin;

use crate::middleware::{Middleware, Next};
use crate::request::Request;
use crate::response::Response;
use crate::routing::method::Method;
use crate::status::StatusCode;

// ── Cors ─────────────────────────────────────────────────────────────────────

/// CORS middleware — injects `Access-Control-*` headers.
#[derive(Clone)]
pub struct Cors {
    allow_origins: Vec<String>,
    allow_methods: Vec<String>,
    allow_headers: Vec<String>,
    max_age: u32,
}

impl Cors {
    /// Create a permissive CORS config (any origin, common methods).
    pub fn permissive() -> Self {
        Self {
            allow_origins: vec!["*".into()],
            allow_methods: vec!["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"]
                .into_iter()
                .map(Into::into)
                .collect(),
            allow_headers: vec!["Content-Type", "Authorization", "Accept"]
                .into_iter()
                .map(Into::into)
                .collect(),
            max_age: 86400,
        }
    }

    /// Restrict to a specific origin.
    pub fn origin(mut self, origin: impl Into<String>) -> Self {
        self.allow_origins = vec![origin.into()];
        self
    }

    /// Set allowed methods.
    pub fn methods(mut self, methods: &[&str]) -> Self {
        self.allow_methods = methods.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set allowed headers.
    pub fn headers(mut self, headers: &[&str]) -> Self {
        self.allow_headers = headers.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set preflight cache duration in seconds.
    pub fn max_age(mut self, seconds: u32) -> Self {
        self.max_age = seconds;
        self
    }

    fn origin_header(&self) -> String {
        self.allow_origins.join(", ")
    }

    fn methods_header(&self) -> String {
        self.allow_methods.join(", ")
    }

    fn headers_header(&self) -> String {
        self.allow_headers.join(", ")
    }

    /// Inject CORS headers into a response.
    fn apply_headers(&self, resp: &mut Response) {
        resp.headers.insert(
            "access-control-allow-origin",
            self.origin_header().into_bytes(),
        );
        resp.headers.insert(
            "access-control-allow-methods",
            self.methods_header().into_bytes(),
        );
        resp.headers.insert(
            "access-control-allow-headers",
            self.headers_header().into_bytes(),
        );
    }
}

impl Middleware for Cors {
    fn handle(&self, req: Request, next: Next) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let cors = self.clone();
        Box::pin(async move {
            // Preflight: respond immediately to OPTIONS
            if req.method == Method::OPTIONS {
                let mut resp = Response::new(StatusCode::NO_CONTENT);
                cors.apply_headers(&mut resp);
                resp.headers.insert(
                    "access-control-max-age",
                    cors.max_age.to_string().into_bytes(),
                );
                return resp;
            }
            // Normal request: forward, then add CORS headers to response
            let mut resp = next.run(req).await;
            cors.apply_headers(&mut resp);
            resp
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_cors_headers() {
        let cors = Cors::permissive();
        assert_eq!(cors.origin_header(), "*");
        assert!(cors.methods_header().contains("GET"));
        assert!(cors.headers_header().contains("Content-Type"));
    }

    #[test]
    fn custom_origin() {
        let cors = Cors::permissive().origin("https://example.com");
        assert_eq!(cors.origin_header(), "https://example.com");
    }
}
