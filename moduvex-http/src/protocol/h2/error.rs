//! HTTP/2 error types (RFC 9113 Section 7).

/// HTTP/2 error codes as defined in RFC 9113 Section 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum H2ErrorCode {
    /// Graceful shutdown or no error.
    NoError = 0x0,
    /// Protocol rule violation detected.
    ProtocolError = 0x1,
    /// Implementation fault.
    InternalError = 0x2,
    /// Flow-control limit exceeded.
    FlowControlError = 0x3,
    /// SETTINGS not acknowledged in time.
    SettingsTimeout = 0x4,
    /// Frame received for closed stream.
    StreamClosed = 0x5,
    /// Frame size constraint violated.
    FrameSizeError = 0x6,
    /// Stream refused before processing.
    RefusedStream = 0x7,
    /// Stream cancelled by the sender.
    Cancel = 0x8,
    /// Compression state failure.
    CompressionError = 0x9,
    /// TCP connection error for CONNECT method.
    ConnectError = 0xa,
    /// Peer generating excessive load.
    EnhanceYourCalm = 0xb,
    /// Transport security requirements not met.
    InadequateSecurity = 0xc,
    /// HTTP/1.1 required for this endpoint.
    Http11Required = 0xd,
}

impl H2ErrorCode {
    /// Convert a raw u32 to an error code, returning `ProtocolError` for unknowns.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0x0 => Self::NoError,
            0x1 => Self::ProtocolError,
            0x2 => Self::InternalError,
            0x3 => Self::FlowControlError,
            0x4 => Self::SettingsTimeout,
            0x5 => Self::StreamClosed,
            0x6 => Self::FrameSizeError,
            0x7 => Self::RefusedStream,
            0x8 => Self::Cancel,
            0x9 => Self::CompressionError,
            0xa => Self::ConnectError,
            0xb => Self::EnhanceYourCalm,
            0xc => Self::InadequateSecurity,
            0xd => Self::Http11Required,
            _ => Self::ProtocolError,
        }
    }
}

/// An HTTP/2 error with an associated stream scope.
///
/// `stream_id == 0` denotes a connection-level error; any other value
/// identifies the affected stream.
#[derive(Debug)]
pub struct H2Error {
    pub code: H2ErrorCode,
    pub message: String,
    /// 0 = connection error, >0 = stream error.
    pub stream_id: u32,
}

impl H2Error {
    /// Create a connection-level error (stream_id = 0).
    pub fn connection(code: H2ErrorCode, msg: impl Into<String>) -> Self {
        Self { code, message: msg.into(), stream_id: 0 }
    }

    /// Create a stream-level error for a specific stream.
    pub fn stream(stream_id: u32, code: H2ErrorCode, msg: impl Into<String>) -> Self {
        Self { code, message: msg.into(), stream_id }
    }
}

impl std::fmt::Display for H2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "H2 error {:?} (stream {}): {}",
            self.code, self.stream_id, self.message
        )
    }
}

impl std::error::Error for H2Error {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_error_has_zero_stream() {
        let e = H2Error::connection(H2ErrorCode::ProtocolError, "bad frame");
        assert_eq!(e.stream_id, 0);
        assert_eq!(e.code, H2ErrorCode::ProtocolError);
    }

    #[test]
    fn stream_error_stores_stream_id() {
        let e = H2Error::stream(3, H2ErrorCode::Cancel, "cancelled");
        assert_eq!(e.stream_id, 3);
        assert_eq!(e.code, H2ErrorCode::Cancel);
    }

    #[test]
    fn display_includes_code_and_message() {
        let e = H2Error::connection(H2ErrorCode::FrameSizeError, "too big");
        let s = e.to_string();
        assert!(s.contains("FrameSizeError"));
        assert!(s.contains("too big"));
    }

    #[test]
    fn from_u32_known_codes() {
        assert_eq!(H2ErrorCode::from_u32(0x0), H2ErrorCode::NoError);
        assert_eq!(H2ErrorCode::from_u32(0x6), H2ErrorCode::FrameSizeError);
        assert_eq!(H2ErrorCode::from_u32(0xd), H2ErrorCode::Http11Required);
    }

    #[test]
    fn from_u32_unknown_maps_to_protocol_error() {
        assert_eq!(H2ErrorCode::from_u32(0xff), H2ErrorCode::ProtocolError);
    }
}
