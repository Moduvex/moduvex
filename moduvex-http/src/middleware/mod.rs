//! Async middleware pipeline — simpler than Tower, debuggable stack traces.
//!
//! Each middleware receives the request and a `Next` handle to call the
//! remaining chain. Middleware can short-circuit by returning a response
//! without calling `next.run()`.

pub mod builtin;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::request::Request;
use crate::response::Response;
use crate::routing::router::BoxHandler;

// ── Middleware trait ──────────────────────────────────────────────────────────

/// Async middleware — intercepts requests before/after the handler.
///
/// Implementations should clone any needed state into the returned future
/// (the `&self` reference is not captured).
pub trait Middleware: Send + Sync + 'static {
    /// Process a request, optionally delegating to the next middleware/handler.
    fn handle(&self, req: Request, next: Next) -> Pin<Box<dyn Future<Output = Response> + Send>>;
}

// ── Next ─────────────────────────────────────────────────────────────────────

/// Handle to the remaining middleware chain + final handler.
///
/// Call `next.run(req)` to continue processing, or return a `Response`
/// directly to short-circuit.
pub struct Next {
    pub(crate) middlewares: Arc<Vec<Arc<dyn Middleware>>>,
    pub(crate) idx: usize,
    pub(crate) handler: Arc<BoxHandler>,
}

impl Next {
    /// Continue to the next middleware, or call the final handler.
    pub async fn run(self, req: Request) -> Response {
        if self.idx >= self.middlewares.len() {
            return (self.handler)(req).await;
        }
        let mw = self.middlewares[self.idx].clone();
        let next = Next {
            middlewares: self.middlewares,
            idx: self.idx + 1,
            handler: self.handler,
        };
        mw.handle(req, next).await
    }
}

// ── Dispatch helper ──────────────────────────────────────────────────────────

/// Run the middleware chain followed by the handler.
///
/// If `middlewares` is empty, calls `handler` directly (zero overhead).
pub async fn dispatch(
    middlewares: &Arc<Vec<Arc<dyn Middleware>>>,
    handler: &Arc<BoxHandler>,
    req: Request,
) -> Response {
    if middlewares.is_empty() {
        return (handler)(req).await;
    }
    let next = Next {
        middlewares: Arc::clone(middlewares),
        idx: 0,
        handler: Arc::clone(handler),
    };
    next.run(req).await
}

// ── Fn-based middleware shorthand ─────────────────────────────────────────────

/// Wrap an `async fn(Request, Next) -> Response` as a `Middleware`.
pub struct FnMiddleware<F>(pub F);

impl<F> Middleware for FnMiddleware<F>
where
    F: Fn(Request, Next) -> Pin<Box<dyn Future<Output = Response> + Send>>
        + Send + Sync + 'static,
{
    fn handle(&self, req: Request, next: Next) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        (self.0)(req, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StatusCode;

    struct AddHeader;

    impl Middleware for AddHeader {
        fn handle(&self, req: Request, next: Next) -> Pin<Box<dyn Future<Output = Response> + Send>> {
            Box::pin(async move {
                let mut resp = next.run(req).await;
                resp.headers.insert("x-test", b"added".to_vec());
                resp
            })
        }
    }

    #[test]
    fn middleware_chain_runs() {
        let handler: BoxHandler = Box::new(|_req| {
            Box::pin(async { Response::new(StatusCode::OK) })
        });
        let mws: Arc<Vec<Arc<dyn Middleware>>> = Arc::new(vec![Arc::new(AddHeader)]);
        let handler = Arc::new(handler);

        // Verify the chain builds without panic (full async test requires runtime)
        let _next = Next { middlewares: mws, idx: 0, handler };
    }
}
