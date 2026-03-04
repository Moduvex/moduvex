//! `Json<T>` extractor — deserialize JSON request body, serialize JSON response.

use serde::de::DeserializeOwned;

use crate::body::Body;
use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::status::StatusCode;

use super::FromRequest;

// ── Json extractor ───────────────────────────────────────────────────────────

/// Extract a JSON body from the request, or produce a JSON response.
///
/// As an extractor: reads the request body and deserializes it into `T`.
/// As a response: serializes `T` and sets `Content-Type: application/json`.
#[derive(Debug)]
pub struct Json<T>(pub T);

impl<T: DeserializeOwned + Send + 'static> FromRequest for Json<T> {
    type Rejection = Response;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        // Validate content-type
        let ct = req.header("content-type").unwrap_or("");
        if !ct.contains("application/json") {
            return Err(
                Response::with_body(StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/json")
                    .content_type("text/plain; charset=utf-8"),
            );
        }

        // Take the body (leaves Body::Empty in its place)
        let body = std::mem::replace(&mut req.body, Body::Empty);
        let bytes = body.into_bytes();

        serde_json::from_slice::<T>(&bytes).map(Json).map_err(|e| {
            Response::with_body(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}"))
                .content_type("text/plain; charset=utf-8")
        })
    }
}

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => Response::json(bytes),
            Err(_) => Response::internal_error(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::method::Method;

    #[test]
    fn json_extract_success() {
        let mut req = Request::new(Method::POST, "/");
        req.headers.insert("content-type", b"application/json".to_vec());
        req.body = Body::from_bytes(b"{\"name\":\"test\"}".to_vec());

        #[derive(serde::Deserialize)]
        struct Payload { name: String }

        let Json(p) = Json::<Payload>::from_request(&mut req).unwrap();
        assert_eq!(p.name, "test");
    }

    #[test]
    fn json_extract_wrong_content_type() {
        let mut req = Request::new(Method::POST, "/");
        req.headers.insert("content-type", b"text/plain".to_vec());
        req.body = Body::from_bytes(b"{}".to_vec());

        #[derive(Debug, serde::Deserialize)]
        struct Payload {}

        let err = Json::<Payload>::from_request(&mut req).unwrap_err();
        assert_eq!(err.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[test]
    fn json_extract_invalid_body() {
        let mut req = Request::new(Method::POST, "/");
        req.headers.insert("content-type", b"application/json".to_vec());
        req.body = Body::from_bytes(b"not json".to_vec());

        #[derive(Debug, serde::Deserialize)]
        struct Payload {}

        let err = Json::<Payload>::from_request(&mut req).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn json_into_response() {
        #[derive(serde::Serialize)]
        struct Out { ok: bool }

        let resp = Json(Out { ok: true }).into_response();
        assert_eq!(resp.status, StatusCode::OK);
        let body = resp.body.into_bytes();
        assert_eq!(body, b"{\"ok\":true}");
    }
}
