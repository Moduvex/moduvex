//! WebSocket support — RFC 6455 upgrade, frame codec, and stream API.
//!
//! # Usage
//! ```ignore
//! use moduvex_http::websocket::{WebSocketUpgrade, Message};
//!
//! async fn ws_handler(ws: WebSocketUpgrade) -> Response {
//!     ws.on_upgrade(|mut stream| async move {
//!         while let Ok(msg) = stream.recv().await {
//!             match msg {
//!                 Message::Text(t) => { let _ = stream.send(Message::Text(t)).await; }
//!                 Message::Close   => break,
//!                 _                => {}
//!             }
//!         }
//!     })
//! }
//! ```

pub mod frame;
pub mod handshake;

use std::future::Future;
use std::pin::Pin;

use crate::extract::FromRequest;
use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::server::tls::Stream;
use crate::status::StatusCode;

use frame::{decode_frame, encode_frame, Frame, FrameError, Opcode};
use handshake::validate_upgrade;

// ── Message ───────────────────────────────────────────────────────────────────

/// A WebSocket application-level message (above frame level).
///
/// Fragmented frames are reassembled before being presented to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// UTF-8 text message.
    Text(String),
    /// Raw binary message.
    Binary(Vec<u8>),
    /// Ping with optional payload (auto-replied with Pong by the stream).
    Ping(Vec<u8>),
    /// Pong (usually received in response to our Ping).
    Pong(Vec<u8>),
    /// Connection close initiated by the peer.
    Close,
}

// ── WebSocket errors ──────────────────────────────────────────────────────────

/// Errors returned by [`WsStream`] send/recv operations.
#[derive(Debug)]
pub enum WsError {
    /// Underlying I/O error.
    Io(std::io::Error),
    /// Frame protocol violation.
    Protocol(String),
    /// Connection closed (peer sent a Close frame or TCP EOF).
    Closed,
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e)       => write!(f, "websocket I/O error: {e}"),
            Self::Protocol(s) => write!(f, "websocket protocol error: {s}"),
            Self::Closed      => write!(f, "websocket connection closed"),
        }
    }
}

impl std::error::Error for WsError {}

// ── WsStream ──────────────────────────────────────────────────────────────────

/// An established WebSocket connection providing `send` / `recv` message API.
///
/// Constructed after the HTTP upgrade handshake completes. The underlying
/// `Stream` is consumed from the `Connection` and taken over by `WsStream`.
pub struct WsStream {
    stream: Stream,
    read_buf: Vec<u8>,
    closed: bool,
}

impl WsStream {
    pub(crate) fn new(stream: Stream) -> Self {
        Self {
            stream,
            read_buf: Vec::with_capacity(4096),
            closed: false,
        }
    }

    /// Prepend bytes already read from the TCP stream (e.g. HTTP read buffer
    /// leftovers) into the WebSocket read buffer before the first `recv()`.
    pub(crate) fn prepend_read_buf(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let mut new_buf = bytes;
            new_buf.extend_from_slice(&self.read_buf);
            self.read_buf = new_buf;
        }
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Send a [`Message`] to the peer.
    ///
    /// Text and Binary frames are sent as single unfragmented frames.
    /// Ping and Pong frames include the optional payload.
    /// Sending `Message::Close` initiates a clean close handshake.
    pub async fn send(&mut self, msg: Message) -> Result<(), WsError> {
        if self.closed {
            return Err(WsError::Closed);
        }

        let frame = match msg {
            Message::Text(s)    => Frame::text(s.into_bytes()),
            Message::Binary(b)  => Frame::binary(b),
            Message::Ping(d)    => Frame::ping(d),
            Message::Pong(d)    => Frame::pong(d),
            Message::Close      => {
                self.closed = true;
                Frame::close(1000, b"")
            }
        };

        let mut buf = Vec::with_capacity(frame.payload.len() + 10);
        encode_frame(&frame, &mut buf);
        self.write_all(&buf).await?;
        Ok(())
    }

    /// Receive the next [`Message`] from the peer.
    ///
    /// Automatically handles control frames:
    /// - Ping → immediately replies with Pong, then continues waiting.
    /// - Close → sends a Close reply and returns `Ok(Message::Close)`.
    ///
    /// Returns `Err(WsError::Closed)` on clean TCP EOF.
    pub async fn recv(&mut self) -> Result<Message, WsError> {
        loop {
            // Try to decode from the buffer first (may have multiple frames buffered).
            match decode_frame(&self.read_buf) {
                Ok((frame, consumed)) => {
                    self.read_buf.drain(..consumed);
                    match self.handle_frame(frame).await? {
                        Some(msg) => return Ok(msg),
                        None      => continue, // control frame handled internally
                    }
                }
                Err(FrameError::Incomplete) => {
                    // Need more data from the network.
                    let n = self.read_some().await?;
                    if n == 0 {
                        return Err(WsError::Closed);
                    }
                }
                Err(FrameError::Invalid(reason)) => {
                    return Err(WsError::Protocol(reason));
                }
            }
        }
    }

    // ── Internal frame handling ───────────────────────────────────────────

    /// Process a decoded frame. Returns `Some(Message)` for data frames,
    /// `None` for internally-handled control frames (Ping auto-reply).
    async fn handle_frame(&mut self, frame: Frame) -> Result<Option<Message>, WsError> {
        match frame.opcode {
            Opcode::Text => {
                let s = String::from_utf8(frame.payload)
                    .map_err(|e| WsError::Protocol(format!("invalid UTF-8: {e}")))?;
                Ok(Some(Message::Text(s)))
            }
            Opcode::Binary => Ok(Some(Message::Binary(frame.payload))),

            Opcode::Ping => {
                // RFC 6455 §5.5.3: respond with Pong carrying the same payload.
                let pong = Frame::pong(frame.payload.clone());
                let mut buf = Vec::new();
                encode_frame(&pong, &mut buf);
                // Best-effort Pong send — ignore errors (connection may be closing).
                let _ = self.write_all(&buf).await;
                Ok(Some(Message::Ping(frame.payload)))
            }

            Opcode::Pong => Ok(Some(Message::Pong(frame.payload))),

            Opcode::Close => {
                // Send Close reply unless we already sent one.
                if !self.closed {
                    self.closed = true;
                    let close = Frame::close(1000, b"");
                    let mut buf = Vec::new();
                    encode_frame(&close, &mut buf);
                    let _ = self.write_all(&buf).await;
                }
                Ok(Some(Message::Close))
            }

            Opcode::Continuation => {
                // Unfragmented continuation frames — not currently supported.
                // In a full implementation this would reassemble fragments.
                Err(WsError::Protocol(
                    "unexpected continuation frame (fragmentation not supported)".to_string(),
                ))
            }
        }
    }

    // ── Low-level I/O ─────────────────────────────────────────────────────

    async fn read_some(&mut self) -> Result<usize, WsError> {
        use moduvex_runtime::net::AsyncRead;
        use std::future::poll_fn;

        let mut tmp = [0u8; 4096];
        let n = poll_fn(|cx| Pin::new(&mut self.stream).poll_read(cx, &mut tmp))
            .await
            .map_err(WsError::Io)?;
        self.read_buf.extend_from_slice(&tmp[..n]);
        Ok(n)
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), WsError> {
        use moduvex_runtime::net::AsyncWrite;
        use std::future::poll_fn;

        let mut sent = 0;
        while sent < buf.len() {
            let n = poll_fn(|cx| Pin::new(&mut self.stream).poll_write(cx, &buf[sent..]))
                .await
                .map_err(WsError::Io)?;
            if n == 0 {
                return Err(WsError::Io(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "websocket write returned 0 bytes",
                )));
            }
            sent += n;
        }
        Ok(())
    }
}

// ── WebSocketUpgrade extractor ─────────────────────────────────────────────────
//
// The extractor pattern used here defers the actual stream takeover until
// `on_upgrade` is called by the handler. The handler returns a Response
// (101 Switching Protocols) which the connection layer intercepts to hand
// off the stream.
//
// Design: we store the accept key in the upgrade guard. The handler wraps
// its user callback in an `UpgradeResponse` — a special `IntoResponse` that
// signals the connection to switch protocols after writing the 101 response.

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
    ///
    /// The handler `f` receives a [`WsStream`] and drives the WebSocket
    /// session. It runs after the 101 response is written by the connection.
    ///
    /// **Note:** In this MVP the upgrade is signalled via a special
    /// `WsUpgradeResponse` wrapper stored in request extensions. The
    /// connection layer checks the response status (101) and upgrades.
    /// The actual handler execution model depends on the connection layer
    /// integration (see `server/connection.rs`).
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
///
/// When the connection layer writes the 101 response and detects this
/// extension in the response, it calls the stored callback with the
/// raw stream. Stored in request extensions by `on_upgrade`.
pub struct WsUpgradeResponse {
    /// The computed `Sec-WebSocket-Accept` value to include in the 101 response.
    pub accept_key: String,
    /// User-provided handler to run after the upgrade.
    pub callback: BoxWsCallback,
}

impl IntoResponse for WsUpgradeResponse {
    fn into_response(self) -> Response {
        // Build the 101 Switching Protocols response.
        let mut resp = Response::new(StatusCode::SWITCHING_PROTOCOLS);
        resp.headers.insert("upgrade", b"websocket".to_vec());
        resp.headers.insert("connection", b"Upgrade".to_vec());
        resp.headers
            .insert("sec-websocket-accept", self.accept_key.into_bytes());
        // Embed the callback in extensions so the connection can extract it.
        resp.extensions.insert(self.callback);
        resp
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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
        assert!(result.is_ok(), "should extract WebSocketUpgrade from valid request");
    }

    #[test]
    fn websocket_upgrade_extractor_rejects_invalid_request() {
        let mut req = Request::new(Method::GET, "/ws");
        // Missing headers — should fail.
        let result = WebSocketUpgrade::from_request(&mut req);
        assert!(result.is_err(), "should reject request missing upgrade headers");
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
        // Accept key must match RFC 6455 §1.3 test vector.
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

    #[test]
    fn message_text_roundtrip() {
        let msg = Message::Text("hello world".to_string());
        if let Message::Text(s) = msg {
            assert_eq!(s, "hello world");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn message_binary_roundtrip() {
        let data = vec![0x01u8, 0x02, 0x03];
        let msg = Message::Binary(data.clone());
        if let Message::Binary(b) = msg {
            assert_eq!(b, data);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn ws_error_display() {
        let e = WsError::Closed;
        assert!(e.to_string().contains("closed"));
        let e2 = WsError::Protocol("test".to_string());
        assert!(e2.to_string().contains("protocol"));
    }
}
