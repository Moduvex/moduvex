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
pub mod stream;
pub mod upgrade;

#[cfg(test)]
mod fragmentation_tests;

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use stream::WsStream;
pub use upgrade::{BoxWsCallback, WebSocketUpgrade, WsRejection, WsUpgradeResponse};

// ── Message ─────────────────────────────────────────────────────────────────

/// A WebSocket application-level message (above frame level).
///
/// Fragmented frames are reassembled transparently before delivery.
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

// ── WebSocket errors ────────────────────────────────────────────────────────

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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
