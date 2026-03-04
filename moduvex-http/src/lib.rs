//! moduvex-http — Custom HTTP/1.1 server built on moduvex-runtime.
//!
//! Zero-copy parsing, type-safe routing, extractors, and middleware pipeline.
//! No external HTTP crates — everything is built from raw TCP primitives.
//!
//! # Quick start
//! ```ignore
//! use moduvex_http::prelude::*;
//!
//! async fn hello(_req: Request) -> Response {
//!     Response::text("Hello, World!")
//! }
//!
//! fn main() {
//!     HttpServer::bind("0.0.0.0:8080")
//!         .get("/", hello)
//!         .serve()
//!         .unwrap();
//! }
//! ```

// ── Modules ───────────────────────────────────────────────────────────────────

pub mod body;
pub mod extract;
pub mod header;
pub mod middleware;
pub mod protocol;
pub mod request;
pub mod response;
pub mod routing;
pub mod server;
pub mod status;

// ── Top-level re-exports ──────────────────────────────────────────────────────

pub use body::{Body, BodyReceiver, BodySender};
pub use extract::{FromRequest, IntoHandler, Json, Path, Query, State};
pub use header::HeaderMap;
pub use middleware::Middleware;
pub use request::{Extensions, HttpVersion, Request};
pub use response::{IntoResponse, Response};
pub use routing::method::Method;
pub use routing::router::Router;
pub use server::HttpServer;
pub use status::StatusCode;

// ── Prelude ───────────────────────────────────────────────────────────────────

/// Convenient glob import for handler authors.
///
/// ```ignore
/// use moduvex_http::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{
        Body, Extensions, FromRequest, HeaderMap, HttpServer, HttpVersion, IntoHandler,
        IntoResponse, Json, Method, Middleware, Path, Query, Request, Response, Router, State,
        StatusCode,
    };
}
