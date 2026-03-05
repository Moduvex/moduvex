//! WebSocket frame codec — RFC 6455 §5.
//!
//! Handles encoding and decoding of WebSocket frames including:
//! - 2-byte base header (FIN, opcode, MASK, payload length)
//! - Extended 16-bit and 64-bit payload lengths
//! - Masking/unmasking (client→server frames are always masked per RFC)
//! - Frame types: Text, Binary, Ping, Pong, Close, Continuation

/// WebSocket frame opcodes (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Continuation = 0x0,
    Text         = 0x1,
    Binary       = 0x2,
    Close        = 0x8,
    Ping         = 0x9,
    Pong         = 0xA,
}

impl Opcode {
    /// Parse a raw 4-bit opcode nibble.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v & 0x0F {
            0x0 => Some(Self::Continuation),
            0x1 => Some(Self::Text),
            0x2 => Some(Self::Binary),
            0x8 => Some(Self::Close),
            0x9 => Some(Self::Ping),
            0xA => Some(Self::Pong),
            _   => None,
        }
    }

    /// Whether this opcode represents a control frame (Close, Ping, Pong).
    pub fn is_control(self) -> bool {
        matches!(self, Self::Close | Self::Ping | Self::Pong)
    }
}

/// A decoded WebSocket frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// FIN bit — true when this is the final fragment.
    pub fin: bool,
    /// Frame opcode.
    pub opcode: Opcode,
    /// Unmasked payload bytes.
    pub payload: Vec<u8>,
}

impl Frame {
    /// Construct a text data frame (FIN=1, no mask — server→client).
    pub fn text(data: impl Into<Vec<u8>>) -> Self {
        Self { fin: true, opcode: Opcode::Text, payload: data.into() }
    }

    /// Construct a binary data frame (FIN=1, no mask — server→client).
    pub fn binary(data: impl Into<Vec<u8>>) -> Self {
        Self { fin: true, opcode: Opcode::Binary, payload: data.into() }
    }

    /// Construct a Ping frame with optional payload (≤125 bytes).
    pub fn ping(data: impl Into<Vec<u8>>) -> Self {
        Self { fin: true, opcode: Opcode::Ping, payload: data.into() }
    }

    /// Construct a Pong frame with optional payload (≤125 bytes).
    pub fn pong(data: impl Into<Vec<u8>>) -> Self {
        Self { fin: true, opcode: Opcode::Pong, payload: data.into() }
    }

    /// Construct a Close frame with an optional status code + reason.
    ///
    /// `code` is a 2-byte big-endian status code (e.g. 1000 = normal closure).
    pub fn close(code: u16, reason: impl AsRef<[u8]>) -> Self {
        let mut payload = Vec::with_capacity(2 + reason.as_ref().len());
        payload.extend_from_slice(&code.to_be_bytes());
        payload.extend_from_slice(reason.as_ref());
        Self { fin: true, opcode: Opcode::Close, payload }
    }
}

// ── Encoding ──────────────────────────────────────────────────────────────────

/// Encode a `Frame` into the output buffer (server→client, no masking).
///
/// Per RFC 6455 §5.1, frames from server to client MUST NOT be masked.
pub fn encode_frame(frame: &Frame, out: &mut Vec<u8>) {
    let payload_len = frame.payload.len();

    // Byte 0: FIN (1 bit) + RSV (3 bits, all 0) + Opcode (4 bits)
    let byte0 = if frame.fin { 0x80 } else { 0x00 } | (frame.opcode as u8);

    // Byte 1: MASK=0 (server→client never masked) + payload length indicator
    if payload_len < 126 {
        out.push(byte0);
        out.push(payload_len as u8);
    } else if payload_len < 65536 {
        out.push(byte0);
        out.push(126);
        out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    } else {
        out.push(byte0);
        out.push(127);
        out.extend_from_slice(&(payload_len as u64).to_be_bytes());
    }

    // Payload (no masking for server→client)
    out.extend_from_slice(&frame.payload);
}

// ── Decoding ──────────────────────────────────────────────────────────────────

/// Errors that can occur when decoding a WebSocket frame.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Not enough data yet — caller should read more and retry.
    Incomplete,
    /// Frame data violates RFC 6455 (e.g. unknown opcode, control frame too large).
    Invalid(String),
}

/// Attempt to decode one frame from `buf`.
///
/// Returns `Ok((frame, consumed_bytes))` on success.
/// Returns `Err(FrameError::Incomplete)` if more data is needed.
/// Bytes consumed are NOT removed from `buf` — caller is responsible.
pub fn decode_frame(buf: &[u8]) -> Result<(Frame, usize), FrameError> {
    if buf.len() < 2 {
        return Err(FrameError::Incomplete);
    }

    let byte0 = buf[0];
    let byte1 = buf[1];

    let fin    = (byte0 & 0x80) != 0;
    let opcode = Opcode::from_u8(byte0 & 0x0F)
        .ok_or_else(|| FrameError::Invalid(format!("unknown opcode: {:#x}", byte0 & 0x0F)))?;

    let masked     = (byte1 & 0x80) != 0;
    let len_field  = (byte1 & 0x7F) as usize;

    // Determine header size and payload length.
    let mut offset = 2usize;
    let payload_len: usize = match len_field {
        0..=125 => len_field,
        126 => {
            if buf.len() < offset + 2 {
                return Err(FrameError::Incomplete);
            }
            let l = u16::from_be_bytes([buf[offset], buf[offset + 1]]) as usize;
            offset += 2;
            l
        }
        127 => {
            if buf.len() < offset + 8 {
                return Err(FrameError::Incomplete);
            }
            let bytes: [u8; 8] = buf[offset..offset + 8].try_into().unwrap();
            let l = u64::from_be_bytes(bytes) as usize;
            offset += 8;
            l
        }
        _ => unreachable!(),
    };

    // Control frames (Close/Ping/Pong) MUST NOT be fragmented and payload ≤ 125 bytes.
    if opcode.is_control() && payload_len > 125 {
        return Err(FrameError::Invalid(
            "control frame payload exceeds 125 bytes".to_string(),
        ));
    }

    // Read masking key (4 bytes) if MASK bit is set.
    let mask_key: Option<[u8; 4]> = if masked {
        if buf.len() < offset + 4 {
            return Err(FrameError::Incomplete);
        }
        let key = [buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]];
        offset += 4;
        Some(key)
    } else {
        None
    };

    // Ensure all payload bytes are available.
    if buf.len() < offset + payload_len {
        return Err(FrameError::Incomplete);
    }

    // Copy and unmask payload.
    let mut payload = buf[offset..offset + payload_len].to_vec();
    if let Some(key) = mask_key {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= key[i % 4];
        }
    }

    let consumed = offset + payload_len;
    Ok((Frame { fin, opcode, payload }, consumed))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Encode / decode roundtrip ─────────────────────────────────────────

    #[test]
    fn encode_decode_text_frame_roundtrip() {
        let frame = Frame::text(b"hello".to_vec());
        let mut buf = Vec::new();
        encode_frame(&frame, &mut buf);

        let (decoded, consumed) = decode_frame(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.opcode, Opcode::Text);
        assert_eq!(decoded.payload, b"hello");
        assert!(decoded.fin);
    }

    #[test]
    fn encode_decode_binary_frame() {
        let data = vec![0x01u8, 0x02, 0x03];
        let frame = Frame::binary(data.clone());
        let mut buf = Vec::new();
        encode_frame(&frame, &mut buf);

        let (decoded, _) = decode_frame(&buf).unwrap();
        assert_eq!(decoded.opcode, Opcode::Binary);
        assert_eq!(decoded.payload, data);
    }

    #[test]
    fn encode_decode_ping_pong() {
        for (f, op) in [
            (Frame::ping(b"data".to_vec()), Opcode::Ping),
            (Frame::pong(b"data".to_vec()), Opcode::Pong),
        ] {
            let mut buf = Vec::new();
            encode_frame(&f, &mut buf);
            let (d, _) = decode_frame(&buf).unwrap();
            assert_eq!(d.opcode, op);
            assert_eq!(d.payload, b"data");
        }
    }

    #[test]
    fn encode_decode_close_frame() {
        let frame = Frame::close(1000, b"normal");
        let mut buf = Vec::new();
        encode_frame(&frame, &mut buf);

        let (decoded, _) = decode_frame(&buf).unwrap();
        assert_eq!(decoded.opcode, Opcode::Close);
        assert_eq!(&decoded.payload[..2], &1000u16.to_be_bytes());
        assert_eq!(&decoded.payload[2..], b"normal");
    }

    // ── Extended payload lengths ──────────────────────────────────────────

    #[test]
    fn encode_decode_16bit_payload_length() {
        // 200 bytes — requires 16-bit extended length field.
        let payload = vec![0xAAu8; 200];
        let frame = Frame::binary(payload.clone());
        let mut buf = Vec::new();
        encode_frame(&frame, &mut buf);

        // Header: 1 + 1 + 2 = 4 bytes
        assert_eq!(buf[1], 126);
        let (decoded, _) = decode_frame(&buf).unwrap();
        assert_eq!(decoded.payload, payload);
    }

    // ── Masking (client→server) ───────────────────────────────────────────

    #[test]
    fn decode_masked_frame() {
        // Build a masked frame manually: "Hello" (0x48 0x65 0x6c 0x6c 0x6f)
        // with mask key [0x37, 0xfa, 0x21, 0x3d]
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        let plaintext = b"Hello";
        let masked: Vec<u8> = plaintext
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect();

        let mut buf = Vec::new();
        buf.push(0x81); // FIN=1, opcode=Text
        buf.push(0x80 | plaintext.len() as u8); // MASK=1, len=5
        buf.extend_from_slice(&mask);
        buf.extend_from_slice(&masked);

        let (frame, consumed) = decode_frame(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(frame.opcode, Opcode::Text);
        assert_eq!(frame.payload, b"Hello");
    }

    // ── Error cases ───────────────────────────────────────────────────────

    #[test]
    fn incomplete_frame_returns_incomplete() {
        // Only 1 byte — need at least 2.
        assert_eq!(decode_frame(&[0x81]), Err(FrameError::Incomplete));
    }

    #[test]
    fn incomplete_payload_returns_incomplete() {
        let frame = Frame::text(b"hello world".to_vec());
        let mut buf = Vec::new();
        encode_frame(&frame, &mut buf);
        // Truncate payload.
        buf.truncate(buf.len() - 3);
        assert_eq!(decode_frame(&buf), Err(FrameError::Incomplete));
    }

    #[test]
    fn invalid_opcode_returns_error() {
        // Opcode 0x3 is reserved/invalid.
        let buf = [0x83u8, 0x00]; // FIN=1, opcode=3, len=0
        let result = decode_frame(&buf);
        assert!(matches!(result, Err(FrameError::Invalid(_))));
    }

    #[test]
    fn control_frame_oversized_payload_returns_error() {
        // Build a ping with 126-byte payload length (invalid per RFC).
        let mut buf = Vec::new();
        buf.push(0x89); // FIN=1, opcode=Ping
        buf.push(0x7E); // len=126 (extended 16-bit)
        buf.extend_from_slice(&126u16.to_be_bytes());
        buf.extend_from_slice(&vec![0u8; 126]);
        let result = decode_frame(&buf);
        assert!(matches!(result, Err(FrameError::Invalid(_))));
    }

    #[test]
    fn opcode_is_control_classification() {
        assert!(Opcode::Close.is_control());
        assert!(Opcode::Ping.is_control());
        assert!(Opcode::Pong.is_control());
        assert!(!Opcode::Text.is_control());
        assert!(!Opcode::Binary.is_control());
        assert!(!Opcode::Continuation.is_control());
    }

    #[test]
    fn empty_text_frame_encodes_and_decodes() {
        let frame = Frame::text(b"".to_vec());
        let mut buf = Vec::new();
        encode_frame(&frame, &mut buf);
        let (decoded, consumed) = decode_frame(&buf).unwrap();
        assert_eq!(consumed, 2);
        assert_eq!(decoded.payload.len(), 0);
    }
}
