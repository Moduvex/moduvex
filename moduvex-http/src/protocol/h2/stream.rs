//! HTTP/2 per-stream state machine (RFC 9113 Section 5.1).
//!
//! Each stream progresses through well-defined lifecycle states. Invalid
//! transitions are rejected with a `StreamClosed` error so the caller can
//! respond with RST_STREAM or GOAWAY as appropriate.

use super::error::{H2Error, H2ErrorCode};

// ── Stream state ──────────────────────────────────────────────────────────────

/// HTTP/2 stream lifecycle states (server-side, no Push Promise).
///
/// ```text
/// Idle ──recv HEADERS──► Open
///                          │
///             recv END_STREAM│      │send END_STREAM
///                          ▼       ▼
///              HalfClosedRemote   HalfClosedLocal
///                          │       │
///             send END_STREAM│      │recv END_STREAM
///                          └───┬───┘
///                              ▼
///                           Closed
/// Any state ──RST_STREAM──► Closed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    /// Not yet opened.
    Idle,
    /// Both sides can send and receive.
    Open,
    /// Client sent END_STREAM; server may still send data.
    HalfClosedRemote,
    /// Server sent END_STREAM; client may still send data.
    HalfClosedLocal,
    /// Stream fully closed; no further frames expected.
    Closed,
}

// ── H2Stream ──────────────────────────────────────────────────────────────────

/// A single HTTP/2 stream owned by the connection manager.
pub struct H2Stream {
    /// Stream identifier (odd for client-initiated).
    pub id: u32,
    /// Current lifecycle state.
    pub state: StreamState,
    /// Remaining bytes the remote peer may send before requiring WINDOW_UPDATE.
    pub recv_window: i64,
    /// Remaining bytes we may send to the remote peer.
    pub send_window: i64,
    /// Accumulated decoded request headers (HPACK output).
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    /// Accumulated request body bytes from DATA frames.
    pub body: Vec<u8>,
    /// True once END_HEADERS has been seen on this stream.
    pub headers_complete: bool,
}

impl H2Stream {
    /// Create a new stream in the `Idle` state with RFC-default window sizes.
    pub fn new(id: u32, initial_window_size: u32) -> Self {
        Self {
            id,
            state: StreamState::Idle,
            recv_window: initial_window_size as i64,
            send_window: initial_window_size as i64,
            headers: Vec::new(),
            body: Vec::new(),
            headers_complete: false,
        }
    }

    /// Transition to `Open` on receipt of a HEADERS frame from the client.
    ///
    /// Valid only from `Idle`; any other state is a `StreamClosed` error.
    pub fn recv_headers(&mut self) -> Result<(), H2Error> {
        match self.state {
            StreamState::Idle => {
                self.state = StreamState::Open;
                Ok(())
            }
            _ => Err(H2Error::stream(
                self.id,
                H2ErrorCode::StreamClosed,
                "HEADERS received on non-idle stream",
            )),
        }
    }

    /// Process an END_STREAM flag received from the client.
    ///
    /// `Open` → `HalfClosedRemote`; `HalfClosedLocal` → `Closed`.
    pub fn recv_end_stream(&mut self) -> Result<(), H2Error> {
        match self.state {
            StreamState::Open => {
                self.state = StreamState::HalfClosedRemote;
                Ok(())
            }
            StreamState::HalfClosedLocal => {
                self.state = StreamState::Closed;
                Ok(())
            }
            _ => Err(H2Error::stream(
                self.id,
                H2ErrorCode::StreamClosed,
                "END_STREAM received in invalid state",
            )),
        }
    }

    /// Record that the server has sent END_STREAM.
    ///
    /// `Open` → `HalfClosedLocal`; `HalfClosedRemote` → `Closed`.
    pub fn send_end_stream(&mut self) -> Result<(), H2Error> {
        match self.state {
            StreamState::Open => {
                self.state = StreamState::HalfClosedLocal;
                Ok(())
            }
            StreamState::HalfClosedRemote => {
                self.state = StreamState::Closed;
                Ok(())
            }
            _ => Err(H2Error::stream(
                self.id,
                H2ErrorCode::StreamClosed,
                "cannot send END_STREAM in current state",
            )),
        }
    }

    /// Forcefully close the stream (RST_STREAM received or sent).
    pub fn reset(&mut self) {
        self.state = StreamState::Closed;
    }

    /// Deduct `len` bytes from the stream receive window.
    ///
    /// Returns `FlowControlError` if the window would go below zero (client
    /// sent more data than allowed).
    pub fn consume_recv_window(&mut self, len: u32) -> Result<(), H2Error> {
        self.recv_window -= len as i64;
        if self.recv_window < 0 {
            return Err(H2Error::stream(
                self.id,
                H2ErrorCode::FlowControlError,
                "stream recv window exceeded",
            ));
        }
        Ok(())
    }

    /// Deduct `len` bytes from the stream send window.
    ///
    /// Returns `FlowControlError` if the window is insufficient.
    pub fn consume_send_window(&mut self, len: u32) -> Result<(), H2Error> {
        self.send_window -= len as i64;
        if self.send_window < 0 {
            return Err(H2Error::stream(
                self.id,
                H2ErrorCode::FlowControlError,
                "stream send window exceeded",
            ));
        }
        Ok(())
    }

    /// Add `increment` to the send window on receipt of a WINDOW_UPDATE.
    ///
    /// Returns `FlowControlError` if the resulting window exceeds 2^31-1.
    pub fn add_send_window(&mut self, increment: u32) -> Result<(), H2Error> {
        const MAX_WINDOW: i64 = (1 << 31) - 1;
        let new_val = self.send_window + increment as i64;
        if new_val > MAX_WINDOW {
            return Err(H2Error::stream(
                self.id,
                H2ErrorCode::FlowControlError,
                "stream send window overflow",
            ));
        }
        self.send_window = new_val;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> H2Stream {
        H2Stream::new(1, 65_535)
    }

    #[test]
    fn idle_to_open_on_recv_headers() {
        let mut s = stream();
        assert_eq!(s.state, StreamState::Idle);
        s.recv_headers().unwrap();
        assert_eq!(s.state, StreamState::Open);
    }

    #[test]
    fn open_to_half_closed_remote_on_recv_end_stream() {
        let mut s = stream();
        s.recv_headers().unwrap();
        s.recv_end_stream().unwrap();
        assert_eq!(s.state, StreamState::HalfClosedRemote);
    }

    #[test]
    fn half_closed_remote_to_closed_on_send_end_stream() {
        let mut s = stream();
        s.recv_headers().unwrap();
        s.recv_end_stream().unwrap();
        s.send_end_stream().unwrap();
        assert_eq!(s.state, StreamState::Closed);
    }

    #[test]
    fn open_to_half_closed_local_on_send_end_stream() {
        let mut s = stream();
        s.recv_headers().unwrap();
        s.send_end_stream().unwrap();
        assert_eq!(s.state, StreamState::HalfClosedLocal);
    }

    #[test]
    fn half_closed_local_to_closed_on_recv_end_stream() {
        let mut s = stream();
        s.recv_headers().unwrap();
        s.send_end_stream().unwrap();
        s.recv_end_stream().unwrap();
        assert_eq!(s.state, StreamState::Closed);
    }

    #[test]
    fn reset_moves_any_state_to_closed() {
        let mut s = stream();
        s.recv_headers().unwrap();
        s.reset();
        assert_eq!(s.state, StreamState::Closed);
    }

    #[test]
    fn recv_headers_on_non_idle_returns_error() {
        let mut s = stream();
        s.recv_headers().unwrap();
        assert!(s.recv_headers().is_err());
    }

    #[test]
    fn consume_recv_window_deducts_correctly() {
        let mut s = stream();
        s.consume_recv_window(1000).unwrap();
        assert_eq!(s.recv_window, 64_535);
    }

    #[test]
    fn consume_recv_window_overflow_is_error() {
        let mut s = stream();
        assert!(s.consume_recv_window(65_536).is_err());
    }

    #[test]
    fn add_send_window_overflow_is_error() {
        let mut s = stream();
        s.send_window = (1 << 31) - 10;
        assert!(s.add_send_window(20).is_err());
    }

    #[test]
    fn consume_send_window_deducts_correctly() {
        let mut s = stream();
        s.consume_send_window(100).unwrap();
        assert_eq!(s.send_window, 65_435);
    }
}
