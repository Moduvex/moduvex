//! WebSocket upgrade extractor and response types.

use std::future::Future;
use std::pin::Pin;

use crate::extract::FromRequest;
use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::status::StatusCode;

use super::handshake::validate_upgrade;
use super::WsStream;

/// Extractor that signals a WebSocket upgrade request.
///
/// Validates the request headers on extraction. Call [`on_upgrade`] to
/// produce the 101 response and schedule the WebSocket handler callback.
///
/// [`on_upgrade`]: WebSocketUpgrade::on_upgrade
pub struct WebSocketUpgrade {
    accept_key: String,
}

/// Rejection returned when WebSocket upgrade validation fails.
#[derive(Debug)]
pub struct WsRejection(pub &'static str);

impl IntoResponse for WsRejection {
    fn into_response(self) -> Response {
        Response::with_body(StatusCode::BAD_REQUEST, self.0)
            .content_type("text/plain; charset=utf-8")
    }
}

impl FromRequest for WebSocketUpgrade {
    type Rejection = WsRejection;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        match validate_upgrade(req) {
            Ok(accept) => Ok(Self { accept_key: accept.accept_key }),
            Err(reason) => Err(WsRejection(reason)),
        }
    }
}

impl WebSocketUpgrade {
    /// Produce the `101 Switching Protocols` response and register the
    /// WebSocket handler callback.
    pub fn on_upgrade<F, Fut>(self, callback: F) -> WsUpgradeResponse
    where
        F: FnOnce(WsStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let raw: RawWsCallback = Box::new(move |stream| Box::pin(callback(stream)));
        WsUpgradeResponse {
            accept_key: self.accept_key,
            callback: BoxWsCallback::new(raw),
        }
    }
}

// ── WsUpgradeResponse ─────────────────────────────────────────────────────────

/// Type alias for the boxed WebSocket callback (raw, not thread-safe).
type RawWsCallback =
    Box<dyn FnOnce(WsStream) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Thread-safe container for the WebSocket upgrade callback.
///
/// Wraps the `FnOnce` callback in a `Mutex<Option<...>>` so it satisfies the
/// `Send + Sync` bounds required by `Extensions::insert`. The callback is
/// extracted once by the connection layer via `Mutex::lock().take()`.
pub struct BoxWsCallback(std::sync::Mutex<Option<RawWsCallback>>);

impl BoxWsCallback {
    fn new(cb: RawWsCallback) -> Self {
        Self(std::sync::Mutex::new(Some(cb)))
    }

    /// Take the callback out of this container (can only be called once).
    pub fn take(self) -> Option<RawWsCallback> {
        self.0.into_inner().ok().flatten()
    }
}

/// A response that signals the connection to upgrade to WebSocket.
pub struct WsUpgradeResponse {
    /// The computed `Sec-WebSocket-Accept` value to include in the 101 response.
    pub accept_key: String,
    /// User-provided handler to run after the upgrade.
    pub callback: BoxWsCallback,
}

impl IntoResponse for WsUpgradeResponse {
    fn into_response(self) -> Response {
        let mut resp = Response::new(StatusCode::SWITCHING_PROTOCOLS);
        resp.headers.insert("upgrade", b"websocket".to_vec());
        resp.headers.insert("connection", b"Upgrade".to_vec());
        resp.headers
            .insert("sec-websocket-accept", self.accept_key.into_bytes());
        resp.extensions.insert(self.callback);
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::method::Method;

    fn make_upgrade_request() -> Request {
        let mut req = Request::new(Method::GET, "/ws");
        req.headers.insert("upgrade", b"websocket".to_vec());
        req.headers.insert("connection", b"Upgrade".to_vec());
        req.headers.insert("sec-websocket-version", b"13".to_vec());
        req.headers
            .insert("sec-websocket-key", b"dGhlIHNhbXBsZSBub25jZQ==".to_vec());
        req
    }

    #[test]
    fn websocket_upgrade_extractor_accepts_valid_request() {
        let mut req = make_upgrade_request();
        let result = WebSocketUpgrade::from_request(&mut req);
        assert!(result.is_ok());
    }

    #[test]
    fn websocket_upgrade_extractor_rejects_invalid_request() {
        let mut req = Request::new(Method::GET, "/ws");
        let result = WebSocketUpgrade::from_request(&mut req);
        assert!(result.is_err());
    }

    #[test]
    fn ws_upgrade_response_is_101() {
        let mut req = make_upgrade_request();
        let upgrade = WebSocketUpgrade::from_request(&mut req).unwrap();
        let resp = upgrade.on_upgrade(|_ws| async {}).into_response();
        assert_eq!(resp.status, StatusCode::SWITCHING_PROTOCOLS);
    }

    #[test]
    fn ws_upgrade_response_has_correct_headers() {
        let mut req = make_upgrade_request();
        let upgrade = WebSocketUpgrade::from_request(&mut req).unwrap();
        let resp = upgrade.on_upgrade(|_ws| async {}).into_response();
        assert_eq!(resp.headers.get_str("upgrade"), Some("websocket"));
        assert_eq!(resp.headers.get_str("connection"), Some("Upgrade"));
        assert_eq!(
            resp.headers.get_str("sec-websocket-accept"),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
    }

    #[test]
    fn ws_rejection_is_400() {
        let rej = WsRejection("missing header").into_response();
        assert_eq!(rej.status, StatusCode::BAD_REQUEST);
    }
}
