//! `State<T>` extractor — access shared application state.

use crate::request::Request;
use crate::response::Response;
use crate::status::StatusCode;

use super::FromRequest;

// ── State extractor ──────────────────────────────────────────────────────────

/// Shared application state injected by the server.
///
/// The server inserts `T` into each request's extensions before dispatch.
/// `State<T>` clones the value out.
#[derive(Debug)]
pub struct State<T>(pub T);

impl<T: Clone + Send + Sync + 'static> FromRequest for State<T> {
    type Rejection = Response;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        req.extensions
            .get::<T>()
            .cloned()
            .map(State)
            .ok_or_else(|| {
                Response::with_body(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "missing application state",
                )
                .content_type("text/plain; charset=utf-8")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::method::Method;

    #[derive(Clone, Debug)]
    struct AppState { db_url: String }

    #[test]
    fn state_extract_present() {
        let mut req = Request::new(Method::GET, "/");
        req.extensions.insert(AppState { db_url: "pg://localhost".into() });
        let State(s) = State::<AppState>::from_request(&mut req).unwrap();
        assert_eq!(s.db_url, "pg://localhost");
    }

    #[test]
    fn state_extract_missing() {
        let mut req = Request::new(Method::GET, "/");
        let err = State::<AppState>::from_request(&mut req).unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
