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

pub mod status;
pub mod header;
pub mod body;
pub mod routing;
pub mod request;
pub mod response;
pub mod protocol;
pub mod server;
pub mod extract;
pub mod middleware;

// ── Top-level re-exports ──────────────────────────────────────────────────────

pub use status::StatusCode;
pub use header::HeaderMap;
pub use body::{Body, BodySender, BodyReceiver};
pub use request::{Request, HttpVersion, Extensions};
pub use response::{Response, IntoResponse};
pub use routing::method::Method;
pub use routing::router::Router;
pub use server::HttpServer;
pub use extract::{FromRequest, IntoHandler, Json, Path, Query, State};
pub use middleware::Middleware;

// ── Prelude ───────────────────────────────────────────────────────────────────

/// Convenient glob import for handler authors.
///
/// ```ignore
/// use moduvex_http::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{
        Body, Extensions, HeaderMap, HttpVersion,
        IntoResponse, Method, Request, Response,
        Router, StatusCode, HttpServer,
        FromRequest, IntoHandler, Json, Path, Query, State,
        Middleware,
    };
}
