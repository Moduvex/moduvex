//! HTTP/2 connection-level flow control (RFC 9113 Section 5.2).
//!
//! Tracks the connection-wide send and receive windows independently from
//! per-stream windows (which are managed on [`super::stream::H2Stream`]).
//!
//! The spec mandates:
//! - Windows are signed 31-bit values; overflow is a `FLOW_CONTROL_ERROR`.
//! - A zero increment on `WINDOW_UPDATE` is a `PROTOCOL_ERROR` (enforced by
//!   the frame parser before we get here).
//! - The receiver should issue `WINDOW_UPDATE` when its window runs low to
//!   avoid stalling the sender.

use super::error::{H2Error, H2ErrorCode};

/// RFC-default initial flow-control window size (65,535 bytes).
pub const DEFAULT_WINDOW_SIZE: u32 = 65_535;

/// Maximum allowed window value per RFC 9113 §6.9.1 (2^31 - 1).
const MAX_WINDOW: i64 = (1 << 31) - 1;

/// Threshold below which `maybe_send_window_update` suggests replenishing the
/// receive window (half of the default window size).
const WINDOW_UPDATE_THRESHOLD: i64 = DEFAULT_WINDOW_SIZE as i64 / 2;

// ── FlowController ────────────────────────────────────────────────────────────

/// Connection-level flow-control state.
///
/// Maintains separate send and receive windows:
/// - `send_window`: how many bytes we may transmit to the peer before stalling.
/// - `recv_window`: how many bytes the peer may send us before we must issue
///   a `WINDOW_UPDATE`.
pub struct FlowController {
    /// Bytes we are allowed to send (decremented when we send DATA).
    pub send_window: i64,
    /// Bytes the peer is allowed to send us (decremented when we recv DATA).
    pub recv_window: i64,
    /// Initial window size negotiated via SETTINGS (used when resetting streams).
    pub initial_window_size: u32,
}

impl FlowController {
    /// Create a new controller with the given initial window size on both sides.
    pub fn new(initial_window_size: u32) -> Self {
        Self {
            send_window: initial_window_size as i64,
            recv_window: initial_window_size as i64,
            initial_window_size,
        }
    }

    /// Deduct `len` bytes from the connection send window.
    ///
    /// Returns `FlowControlError` if the window would go negative (i.e. we
    /// must not send more than the peer's advertised window allows).
    pub fn consume_send(&mut self, len: u32) -> Result<(), H2Error> {
        self.send_window -= len as i64;
        if self.send_window < 0 {
            return Err(H2Error::connection(
                H2ErrorCode::FlowControlError,
                "connection send window exceeded",
            ));
        }
        Ok(())
    }

    /// Deduct `len` bytes from the connection receive window.
    ///
    /// Returns `FlowControlError` if the window would go negative (peer sent
    /// more data than its window allows — a protocol violation).
    pub fn consume_recv(&mut self, len: u32) -> Result<(), H2Error> {
        self.recv_window -= len as i64;
        if self.recv_window < 0 {
            return Err(H2Error::connection(
                H2ErrorCode::FlowControlError,
                "connection recv window exceeded",
            ));
        }
        Ok(())
    }

    /// Apply a WINDOW_UPDATE increment to the connection send window
    /// (stream_id == 0 on the wire).
    ///
    /// Returns `FlowControlError` if the resulting window would exceed 2^31-1.
    pub fn window_update(&mut self, increment: u32) -> Result<(), H2Error> {
        let new_val = self.send_window + increment as i64;
        if new_val > MAX_WINDOW {
            return Err(H2Error::connection(
                H2ErrorCode::FlowControlError,
                "connection send window overflow",
            ));
        }
        self.send_window = new_val;
        Ok(())
    }

    /// Return the increment to advertise in a WINDOW_UPDATE frame if the
    /// receive window has dropped below the replenish threshold.
    ///
    /// Resets the receive window to the initial size. Returns `None` if the
    /// window is still healthy and no update is needed.
    pub fn maybe_send_window_update(&mut self) -> Option<u32> {
        if self.recv_window <= WINDOW_UPDATE_THRESHOLD {
            let increment = (self.initial_window_size as i64 - self.recv_window) as u32;
            self.recv_window = self.initial_window_size as i64;
            Some(increment)
        } else {
            None
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fc() -> FlowController {
        FlowController::new(DEFAULT_WINDOW_SIZE)
    }

    #[test]
    fn initial_windows_match_param() {
        let fc = FlowController::new(1024);
        assert_eq!(fc.send_window, 1024);
        assert_eq!(fc.recv_window, 1024);
    }

    #[test]
    fn consume_send_deducts_bytes() {
        let mut fc = fc();
        fc.consume_send(1000).unwrap();
        assert_eq!(fc.send_window, (DEFAULT_WINDOW_SIZE - 1000) as i64);
    }

    #[test]
    fn consume_send_overflow_returns_error() {
        let mut fc = fc();
        assert!(fc.consume_send(DEFAULT_WINDOW_SIZE + 1).is_err());
    }

    #[test]
    fn consume_recv_deducts_bytes() {
        let mut fc = fc();
        fc.consume_recv(500).unwrap();
        assert_eq!(fc.recv_window, (DEFAULT_WINDOW_SIZE - 500) as i64);
    }

    #[test]
    fn consume_recv_overflow_returns_error() {
        let mut fc = fc();
        assert!(fc.consume_recv(DEFAULT_WINDOW_SIZE + 1).is_err());
    }

    #[test]
    fn window_update_increases_send_window() {
        let mut fc = fc();
        fc.consume_send(1000).unwrap();
        fc.window_update(1000).unwrap();
        assert_eq!(fc.send_window, DEFAULT_WINDOW_SIZE as i64);
    }

    #[test]
    fn window_update_overflow_returns_error() {
        let mut fc = fc();
        fc.send_window = MAX_WINDOW - 10;
        assert!(fc.window_update(20).is_err());
    }

    #[test]
    fn maybe_send_window_update_none_when_healthy() {
        let mut fc = fc();
        fc.consume_recv(100).unwrap();
        // Window still well above threshold
        assert!(fc.maybe_send_window_update().is_none());
    }

    #[test]
    fn maybe_send_window_update_some_when_low() {
        let mut fc = fc();
        // Consume most of the window to fall below threshold
        let drain = DEFAULT_WINDOW_SIZE - WINDOW_UPDATE_THRESHOLD as u32 + 1;
        fc.consume_recv(drain).unwrap();
        let update = fc.maybe_send_window_update();
        assert!(update.is_some());
        // After update the receive window is restored to initial size
        assert_eq!(fc.recv_window, DEFAULT_WINDOW_SIZE as i64);
    }
}
