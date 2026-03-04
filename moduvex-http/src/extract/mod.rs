//! Type-safe request data extraction — inspired by Axum's extractor pattern.
//!
//! `FromRequest` is the core trait. `IntoHandler` bridges extractor-aware
//! functions into the router's type-erased `BoxHandler`.

use std::future::Future;
use std::pin::Pin;

use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::routing::method::Method;
use crate::routing::router::BoxHandler;

pub mod json;
pub mod path_params;
pub mod query;
pub mod state;

// ── FromRequest trait ────────────────────────────────────────────────────────

/// Extract typed data from an HTTP request.
///
/// Extraction is synchronous — it runs before the async handler is invoked.
/// Use `&mut Request` so body-consuming extractors (e.g. `Json<T>`) can take
/// the body via `std::mem::take`, while header/query extractors just read.
pub trait FromRequest: Sized + Send + 'static {
    /// Error type returned when extraction fails.
    type Rejection: IntoResponse + Send + 'static;

    /// Attempt to extract `Self` from the request.
    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection>;
}

// Request itself is trivially extractable — backward compatible with
// `async fn handler(req: Request) -> Response` signatures.
impl FromRequest for Request {
    type Rejection = std::convert::Infallible;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        Ok(std::mem::replace(req, Request::new(Method::GET, "/")))
    }
}

impl IntoResponse for std::convert::Infallible {
    fn into_response(self) -> Response { match self {} }
}

// ── IntoHandler trait ────────────────────────────────────────────────────────

/// Convert a handler function (with extractor arguments) into a `BoxHandler`.
///
/// `T` is a marker type (tuple of extractors) for type inference — it is never
/// constructed at runtime.
pub trait IntoHandler<T>: Send + Sync + 'static {
    /// Consume self and produce a type-erased handler.
    fn into_box_handler(self) -> BoxHandler;
}

// ── Macro: generate impls for 0..8 extractor args ────────────────────────────

macro_rules! impl_handler {
    // 0 args
    () => {
        impl<Func, Fut, Res> IntoHandler<()> for Func
        where
            Func: Fn() -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send + 'static,
            Res: IntoResponse + 'static,
        {
            fn into_box_handler(self) -> BoxHandler {
                let f = self;
                Box::new(move |_req| {
                    let fut = f();
                    Box::pin(async move { fut.await.into_response() })
                })
            }
        }
    };
    // N args
    ($($T:ident),+) => {
        #[allow(non_snake_case)]
        impl<Func, Fut, Res, $($T),+> IntoHandler<($($T,)+)> for Func
        where
            Func: Fn($($T),+) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send + 'static,
            Res: IntoResponse + 'static,
            $($T: FromRequest,)+
        {
            fn into_box_handler(self) -> BoxHandler {
                let f = self;
                Box::new(move |mut req: Request| {
                    $(
                        let $T = match $T::from_request(&mut req) {
                            Ok(v) => v,
                            Err(rej) => return Box::pin(async move { rej.into_response() })
                                as Pin<Box<dyn Future<Output = Response> + Send + 'static>>,
                        };
                    )+
                    let fut = f($($T),+);
                    Box::pin(async move { fut.await.into_response() })
                })
            }
        }
    };
}

impl_handler!();
impl_handler!(T1);
impl_handler!(T1, T2);
impl_handler!(T1, T2, T3);
impl_handler!(T1, T2, T3, T4);
impl_handler!(T1, T2, T3, T4, T5);
impl_handler!(T1, T2, T3, T4, T5, T6);
impl_handler!(T1, T2, T3, T4, T5, T6, T7);
impl_handler!(T1, T2, T3, T4, T5, T6, T7, T8);

// ── Re-exports ───────────────────────────────────────────────────────────────

pub use json::Json;
pub use path_params::Path;
pub use query::Query;
pub use state::State;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::Response;
    use crate::status::StatusCode;

    async fn no_args() -> &'static str { "hello" }

    async fn one_arg(_req: Request) -> Response {
        Response::new(StatusCode::OK)
    }

    #[test]
    fn zero_arg_handler_compiles() {
        let _bh: BoxHandler = no_args.into_box_handler();
    }

    #[test]
    fn single_arg_handler_compiles() {
        let _bh: BoxHandler = one_arg.into_box_handler();
    }
}
