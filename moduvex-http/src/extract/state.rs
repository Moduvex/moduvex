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
    struct AppState {
        db_url: String,
    }

    #[test]
    fn state_extract_present() {
        let mut req = Request::new(Method::GET, "/");
        req.extensions.insert(AppState {
            db_url: "pg://localhost".into(),
        });
        let State(s) = State::<AppState>::from_request(&mut req).unwrap();
        assert_eq!(s.db_url, "pg://localhost");
    }

    #[test]
    fn state_extract_missing() {
        let mut req = Request::new(Method::GET, "/");
        let err = State::<AppState>::from_request(&mut req).unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn state_extract_clones_value() {
        // State extractor should clone the value, not move it
        let mut req = Request::new(Method::GET, "/");
        req.extensions.insert(AppState {
            db_url: "pg://cloned".into(),
        });
        let State(s1) = State::<AppState>::from_request(&mut req).unwrap();
        // Extensions still has the value (State clones, not moves)
        let s2 = req.extensions.get::<AppState>().unwrap();
        assert_eq!(s1.db_url, "pg://cloned");
        assert_eq!(s2.db_url, "pg://cloned");
    }

    #[test]
    fn state_extract_multiple_types_independently() {
        #[derive(Clone, Debug)]
        struct Config {
            port: u16,
        }

        let mut req = Request::new(Method::GET, "/");
        req.extensions.insert(AppState {
            db_url: "pg://localhost".into(),
        });
        req.extensions.insert(Config { port: 8080 });

        let State(app) = State::<AppState>::from_request(&mut req).unwrap();
        let State(cfg) = State::<Config>::from_request(&mut req).unwrap();
        assert_eq!(app.db_url, "pg://localhost");
        assert_eq!(cfg.port, 8080);
    }

    #[test]
    fn state_extract_primitive_type() {
        let mut req = Request::new(Method::GET, "/");
        req.extensions.insert(42u32);
        let State(n) = State::<u32>::from_request(&mut req).unwrap();
        assert_eq!(n, 42);
    }
}
