//! HTTP/2 frame codec — parser and encoder (RFC 9113 Section 4).
//!
//! Frame wire format (9-byte fixed header):
//! ```text
//! +-----------------------------------------------+
//! |                 Length (24)                    |
//! +---------------+---------------+----------------+
//! |   Type (8)    |   Flags (8)   |
//! +-+-------------+---------------+----------------+
//! |R|          Stream Identifier (31)              |
//! +=+===============================================+
//! |              Frame Payload (0...)             ...
//! +-----------------------------------------------+
//! ```

use super::error::{H2Error, H2ErrorCode};

// ── Frame type constants ───────────────────────────────────────────────────────

/// HTTP/2 frame type byte values (RFC 9113 Section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Data = 0x0,
    Headers = 0x1,
    Priority = 0x2,
    RstStream = 0x3,
    Settings = 0x4,
    PushPromise = 0x5,
    Ping = 0x6,
    Goaway = 0x7,
    WindowUpdate = 0x8,
    Continuation = 0x9,
}

impl FrameType {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x0 => Some(Self::Data),
            0x1 => Some(Self::Headers),
            0x2 => Some(Self::Priority),
            0x3 => Some(Self::RstStream),
            0x4 => Some(Self::Settings),
            0x5 => Some(Self::PushPromise),
            0x6 => Some(Self::Ping),
            0x7 => Some(Self::Goaway),
            0x8 => Some(Self::WindowUpdate),
            0x9 => Some(Self::Continuation),
            _ => None,
        }
    }
}

// ── Flag bit constants ────────────────────────────────────────────────────────

/// Common flag bit masks used across frame types.
pub mod flags {
    /// DATA / HEADERS: no more frames for this stream.
    pub const END_STREAM: u8 = 0x1;
    /// SETTINGS / PING: acknowledgement frame.
    pub const ACK: u8 = 0x1;
    /// HEADERS / PUSH_PROMISE / CONTINUATION: header block is complete.
    pub const END_HEADERS: u8 = 0x4;
    /// HEADERS / DATA / PUSH_PROMISE: payload is padded.
    pub const PADDED: u8 = 0x8;
    /// HEADERS: priority fields are present.
    pub const PRIORITY_FLAG: u8 = 0x20;
}

// ── SETTINGS parameter IDs ────────────────────────────────────────────────────

pub const SETTINGS_HEADER_TABLE_SIZE: u16 = 0x1;
pub const SETTINGS_ENABLE_PUSH: u16 = 0x2;
pub const SETTINGS_MAX_CONCURRENT_STREAMS: u16 = 0x3;
pub const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
pub const SETTINGS_MAX_FRAME_SIZE: u16 = 0x5;
pub const SETTINGS_MAX_HEADER_LIST_SIZE: u16 = 0x6;

// ── Frame header ──────────────────────────────────────────────────────────────

/// Parsed 9-byte frame header.
#[derive(Debug, Clone)]
pub struct FrameHeader {
    /// Payload length in bytes (24-bit field).
    pub length: u32,
    /// Raw frame type byte (unknown types pass through).
    pub frame_type: u8,
    pub flags: u8,
    /// Stream identifier with reserved bit masked out.
    pub stream_id: u32,
}

/// Parse a 9-byte frame header from `buf`.
///
/// Returns `None` when fewer than 9 bytes are available.
pub fn parse_frame_header(buf: &[u8]) -> Option<FrameHeader> {
    if buf.len() < 9 {
        return None;
    }
    let length = (buf[0] as u32) << 16 | (buf[1] as u32) << 8 | buf[2] as u32;
    let frame_type = buf[3];
    let flags = buf[4];
    // Mask reserved bit (MSB of stream identifier field).
    let stream_id = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff;
    Some(FrameHeader { length, frame_type, flags, stream_id })
}

// ── Typed frame variants ──────────────────────────────────────────────────────

/// Fully-parsed, typed HTTP/2 frame.
#[derive(Debug, Clone)]
pub enum Frame {
    Data {
        stream_id: u32,
        end_stream: bool,
        payload: Vec<u8>,
    },
    Headers {
        stream_id: u32,
        end_stream: bool,
        end_headers: bool,
        header_block: Vec<u8>,
    },
    Priority {
        stream_id: u32,
        exclusive: bool,
        dependency: u32,
        weight: u8,
    },
    RstStream {
        stream_id: u32,
        error_code: u32,
    },
    Settings {
        ack: bool,
        /// List of `(identifier, value)` pairs.
        values: Vec<(u16, u32)>,
    },
    PushPromise {
        stream_id: u32,
        promised_id: u32,
        header_block: Vec<u8>,
    },
    Ping {
        ack: bool,
        data: [u8; 8],
    },
    Goaway {
        last_stream_id: u32,
        error_code: u32,
        debug_data: Vec<u8>,
    },
    WindowUpdate {
        stream_id: u32,
        increment: u32,
    },
    Continuation {
        stream_id: u32,
        end_headers: bool,
        header_block: Vec<u8>,
    },
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a typed `Frame` from a validated header and its payload bytes.
///
/// `payload` must be exactly `header.length` bytes long.
pub fn parse_frame(header: &FrameHeader, payload: &[u8]) -> Result<Frame, H2Error> {
    let sid = header.stream_id;
    let fl = header.flags;

    match FrameType::from_u8(header.frame_type) {
        Some(FrameType::Data) => parse_data(sid, fl, payload),
        Some(FrameType::Headers) => parse_headers(sid, fl, payload),
        Some(FrameType::Priority) => parse_priority(sid, payload),
        Some(FrameType::RstStream) => parse_rst_stream(sid, payload),
        Some(FrameType::Settings) => parse_settings(fl, payload),
        Some(FrameType::PushPromise) => parse_push_promise(sid, fl, payload),
        Some(FrameType::Ping) => parse_ping(fl, payload),
        Some(FrameType::Goaway) => parse_goaway(sid, payload),
        Some(FrameType::WindowUpdate) => parse_window_update(sid, payload),
        Some(FrameType::Continuation) => parse_continuation(sid, fl, payload),
        // Unknown frame types MUST be ignored per RFC 9113 §5.5.
        None => Ok(Frame::Data {
            stream_id: sid,
            end_stream: false,
            payload: payload.to_vec(),
        }),
    }
}

fn parse_data(sid: u32, fl: u8, payload: &[u8]) -> Result<Frame, H2Error> {
    if sid == 0 {
        return Err(H2Error::connection(H2ErrorCode::ProtocolError, "DATA on stream 0"));
    }
    let body = strip_padding(fl, payload)?;
    Ok(Frame::Data {
        stream_id: sid,
        end_stream: fl & flags::END_STREAM != 0,
        payload: body.to_vec(),
    })
}

fn parse_headers(sid: u32, fl: u8, payload: &[u8]) -> Result<Frame, H2Error> {
    if sid == 0 {
        return Err(H2Error::connection(H2ErrorCode::ProtocolError, "HEADERS on stream 0"));
    }
    let mut data = strip_padding(fl, payload)?;

    // Skip PRIORITY fields (5 bytes) when PRIORITY flag is set.
    if fl & flags::PRIORITY_FLAG != 0 {
        if data.len() < 5 {
            return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "HEADERS priority underflow"));
        }
        data = &data[5..];
    }
    Ok(Frame::Headers {
        stream_id: sid,
        end_stream: fl & flags::END_STREAM != 0,
        end_headers: fl & flags::END_HEADERS != 0,
        header_block: data.to_vec(),
    })
}

fn parse_priority(sid: u32, payload: &[u8]) -> Result<Frame, H2Error> {
    if payload.len() != 5 {
        return Err(H2Error::stream(sid, H2ErrorCode::FrameSizeError, "PRIORITY must be 5 bytes"));
    }
    let raw = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let exclusive = raw & 0x8000_0000 != 0;
    let dependency = raw & 0x7fff_ffff;
    let weight = payload[4];
    Ok(Frame::Priority { stream_id: sid, exclusive, dependency, weight })
}

fn parse_rst_stream(sid: u32, payload: &[u8]) -> Result<Frame, H2Error> {
    if payload.len() != 4 {
        return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "RST_STREAM must be 4 bytes"));
    }
    let error_code = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    Ok(Frame::RstStream { stream_id: sid, error_code })
}

fn parse_settings(fl: u8, payload: &[u8]) -> Result<Frame, H2Error> {
    let ack = fl & flags::ACK != 0;
    if ack && !payload.is_empty() {
        return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "SETTINGS ACK must be empty"));
    }
    if payload.len() % 6 != 0 {
        return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "SETTINGS length not multiple of 6"));
    }
    let mut values = Vec::with_capacity(payload.len() / 6);
    let mut pos = 0;
    while pos + 6 <= payload.len() {
        let id = u16::from_be_bytes([payload[pos], payload[pos + 1]]);
        let val = u32::from_be_bytes([payload[pos + 2], payload[pos + 3], payload[pos + 4], payload[pos + 5]]);
        values.push((id, val));
        pos += 6;
    }
    Ok(Frame::Settings { ack, values })
}

fn parse_push_promise(sid: u32, fl: u8, payload: &[u8]) -> Result<Frame, H2Error> {
    if sid == 0 {
        return Err(H2Error::connection(H2ErrorCode::ProtocolError, "PUSH_PROMISE on stream 0"));
    }
    let data = strip_padding(fl, payload)?;
    if data.len() < 4 {
        return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "PUSH_PROMISE too short"));
    }
    let promised_id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) & 0x7fff_ffff;
    Ok(Frame::PushPromise {
        stream_id: sid,
        promised_id,
        header_block: data[4..].to_vec(),
    })
}

fn parse_ping(fl: u8, payload: &[u8]) -> Result<Frame, H2Error> {
    if payload.len() != 8 {
        return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "PING must be 8 bytes"));
    }
    let mut data = [0u8; 8];
    data.copy_from_slice(payload);
    Ok(Frame::Ping { ack: fl & flags::ACK != 0, data })
}

fn parse_goaway(sid: u32, payload: &[u8]) -> Result<Frame, H2Error> {
    if sid != 0 {
        return Err(H2Error::connection(H2ErrorCode::ProtocolError, "GOAWAY must use stream 0"));
    }
    if payload.len() < 8 {
        return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "GOAWAY too short"));
    }
    let last_stream_id = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) & 0x7fff_ffff;
    let error_code = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    Ok(Frame::Goaway {
        last_stream_id,
        error_code,
        debug_data: payload[8..].to_vec(),
    })
}

fn parse_window_update(sid: u32, payload: &[u8]) -> Result<Frame, H2Error> {
    if payload.len() != 4 {
        return Err(H2Error::connection(H2ErrorCode::FrameSizeError, "WINDOW_UPDATE must be 4 bytes"));
    }
    let increment = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) & 0x7fff_ffff;
    if increment == 0 {
        let err = if sid == 0 {
            H2Error::connection(H2ErrorCode::ProtocolError, "WINDOW_UPDATE increment must be > 0")
        } else {
            H2Error::stream(sid, H2ErrorCode::ProtocolError, "WINDOW_UPDATE increment must be > 0")
        };
        return Err(err);
    }
    Ok(Frame::WindowUpdate { stream_id: sid, increment })
}

fn parse_continuation(sid: u32, fl: u8, payload: &[u8]) -> Result<Frame, H2Error> {
    if sid == 0 {
        return Err(H2Error::connection(H2ErrorCode::ProtocolError, "CONTINUATION on stream 0"));
    }
    Ok(Frame::Continuation {
        stream_id: sid,
        end_headers: fl & flags::END_HEADERS != 0,
        header_block: payload.to_vec(),
    })
}

/// Strip optional padding (RFC 9113 §6.1). Returns the unpadded slice.
fn strip_padding(fl: u8, payload: &[u8]) -> Result<&[u8], H2Error> {
    if fl & flags::PADDED == 0 {
        return Ok(payload);
    }
    if payload.is_empty() {
        return Err(H2Error::connection(H2ErrorCode::ProtocolError, "PADDED flag but empty payload"));
    }
    let pad_len = payload[0] as usize;
    let total = 1 + pad_len;
    if total > payload.len() {
        return Err(H2Error::connection(H2ErrorCode::ProtocolError, "padding exceeds payload length"));
    }
    Ok(&payload[1..payload.len() - pad_len])
}

// ── Encoder ───────────────────────────────────────────────────────────────────

/// Encode a `Frame` into `out` (9-byte header followed by payload).
pub fn encode_frame(frame: &Frame, out: &mut Vec<u8>) {
    match frame {
        Frame::Data { stream_id, end_stream, payload } => {
            let fl = if *end_stream { flags::END_STREAM } else { 0 };
            write_header(out, payload.len() as u32, FrameType::Data as u8, fl, *stream_id);
            out.extend_from_slice(payload);
        }
        Frame::Headers { stream_id, end_stream, end_headers, header_block } => {
            let mut fl = 0u8;
            if *end_stream { fl |= flags::END_STREAM; }
            if *end_headers { fl |= flags::END_HEADERS; }
            write_header(out, header_block.len() as u32, FrameType::Headers as u8, fl, *stream_id);
            out.extend_from_slice(header_block);
        }
        Frame::Priority { stream_id, exclusive, dependency, weight } => {
            write_header(out, 5, FrameType::Priority as u8, 0, *stream_id);
            let raw = if *exclusive { dependency | 0x8000_0000 } else { *dependency };
            out.extend_from_slice(&raw.to_be_bytes());
            out.push(*weight);
        }
        Frame::RstStream { stream_id, error_code } => {
            write_header(out, 4, FrameType::RstStream as u8, 0, *stream_id);
            out.extend_from_slice(&error_code.to_be_bytes());
        }
        Frame::Settings { ack, values } => {
            let payload_len = values.len() as u32 * 6;
            let fl = if *ack { flags::ACK } else { 0 };
            write_header(out, payload_len, FrameType::Settings as u8, fl, 0);
            for (id, val) in values {
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&val.to_be_bytes());
            }
        }
        Frame::PushPromise { stream_id, promised_id, header_block } => {
            let payload_len = 4 + header_block.len() as u32;
            write_header(out, payload_len, FrameType::PushPromise as u8, flags::END_HEADERS, *stream_id);
            out.extend_from_slice(&(promised_id & 0x7fff_ffff).to_be_bytes());
            out.extend_from_slice(header_block);
        }
        Frame::Ping { ack, data } => {
            let fl = if *ack { flags::ACK } else { 0 };
            write_header(out, 8, FrameType::Ping as u8, fl, 0);
            out.extend_from_slice(data);
        }
        Frame::Goaway { last_stream_id, error_code, debug_data } => {
            let payload_len = 8 + debug_data.len() as u32;
            write_header(out, payload_len, FrameType::Goaway as u8, 0, 0);
            out.extend_from_slice(&(last_stream_id & 0x7fff_ffff).to_be_bytes());
            out.extend_from_slice(&error_code.to_be_bytes());
            out.extend_from_slice(debug_data);
        }
        Frame::WindowUpdate { stream_id, increment } => {
            write_header(out, 4, FrameType::WindowUpdate as u8, 0, *stream_id);
            out.extend_from_slice(&(increment & 0x7fff_ffff).to_be_bytes());
        }
        Frame::Continuation { stream_id, end_headers, header_block } => {
            let fl = if *end_headers { flags::END_HEADERS } else { 0 };
            write_header(out, header_block.len() as u32, FrameType::Continuation as u8, fl, *stream_id);
            out.extend_from_slice(header_block);
        }
    }
}

/// Write the 9-byte frame header into `out`.
fn write_header(out: &mut Vec<u8>, length: u32, frame_type: u8, flags: u8, stream_id: u32) {
    out.push((length >> 16) as u8);
    out.push((length >> 8) as u8);
    out.push(length as u8);
    out.push(frame_type);
    out.push(flags);
    out.extend_from_slice(&(stream_id & 0x7fff_ffff).to_be_bytes());
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode then parse; verifies the round-trip produces an identical frame.
    fn round_trip(frame: Frame) -> Frame {
        let mut buf = Vec::new();
        encode_frame(&frame, &mut buf);
        let header = parse_frame_header(&buf).expect("header parse failed");
        let payload = &buf[9..9 + header.length as usize];
        parse_frame(&header, payload).expect("frame parse failed")
    }

    // ── Header parsing ────────────────────────────────────────────────────

    #[test]
    fn parse_header_needs_nine_bytes() {
        assert!(parse_frame_header(&[0u8; 8]).is_none());
        assert!(parse_frame_header(&[0u8; 9]).is_some());
    }

    #[test]
    fn parse_header_masks_reserved_bit() {
        // Set reserved bit (MSB of stream-id field).
        let mut buf = [0u8; 9];
        buf[5] = 0x80; // reserved bit set
        let h = parse_frame_header(&buf).unwrap();
        assert_eq!(h.stream_id, 0);
    }

    #[test]
    fn parse_header_length_field() {
        let mut buf = [0u8; 9];
        buf[0] = 0x00;
        buf[1] = 0x00;
        buf[2] = 0x0c; // length = 12
        let h = parse_frame_header(&buf).unwrap();
        assert_eq!(h.length, 12);
    }

    // ── DATA ──────────────────────────────────────────────────────────────

    #[test]
    fn data_round_trip() {
        let f = Frame::Data { stream_id: 1, end_stream: true, payload: b"hello".to_vec() };
        if let Frame::Data { stream_id, end_stream, payload } = round_trip(f) {
            assert_eq!(stream_id, 1);
            assert!(end_stream);
            assert_eq!(payload, b"hello");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn data_stream_zero_rejected() {
        let mut buf = Vec::new();
        encode_frame(&Frame::Data { stream_id: 1, end_stream: false, payload: vec![] }, &mut buf);
        // Patch stream-id to 0.
        buf[5] = 0; buf[6] = 0; buf[7] = 0; buf[8] = 0;
        let h = parse_frame_header(&buf).unwrap();
        assert!(parse_frame(&h, &[]).is_err());
    }

    // ── SETTINGS ──────────────────────────────────────────────────────────

    #[test]
    fn settings_round_trip() {
        let vals = vec![
            (SETTINGS_HEADER_TABLE_SIZE, 4096u32),
            (SETTINGS_MAX_FRAME_SIZE, 16384),
        ];
        let f = Frame::Settings { ack: false, values: vals.clone() };
        if let Frame::Settings { ack, values } = round_trip(f) {
            assert!(!ack);
            assert_eq!(values, vals);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn settings_ack_round_trip() {
        let f = Frame::Settings { ack: true, values: vec![] };
        if let Frame::Settings { ack, values } = round_trip(f) {
            assert!(ack);
            assert!(values.is_empty());
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn settings_invalid_length_rejected() {
        let mut buf = Vec::new();
        encode_frame(&Frame::Settings { ack: false, values: vec![(1, 100)] }, &mut buf);
        // Corrupt payload length to 5 (not multiple of 6).
        buf[2] = 5;
        let h = parse_frame_header(&buf).unwrap();
        assert!(parse_frame(&h, &buf[9..14]).is_err());
    }

    #[test]
    fn settings_ack_with_payload_rejected() {
        // SETTINGS ACK must have empty payload.
        let fl: u8 = flags::ACK;
        let payload = [0u8; 6]; // one setting entry
        let mut buf = Vec::new();
        write_header(&mut buf, 6, FrameType::Settings as u8, fl, 0);
        buf.extend_from_slice(&payload);
        let h = parse_frame_header(&buf).unwrap();
        assert!(parse_frame(&h, &payload).is_err());
    }

    // ── PING ──────────────────────────────────────────────────────────────

    #[test]
    fn ping_round_trip() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let f = Frame::Ping { ack: false, data };
        if let Frame::Ping { ack, data: d } = round_trip(f) {
            assert!(!ack);
            assert_eq!(d, data);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn ping_wrong_size_rejected() {
        let mut buf = Vec::new();
        write_header(&mut buf, 4, FrameType::Ping as u8, 0, 0);
        buf.extend_from_slice(&[0u8; 4]);
        let h = parse_frame_header(&buf).unwrap();
        assert!(parse_frame(&h, &[0u8; 4]).is_err());
    }

    // ── GOAWAY ────────────────────────────────────────────────────────────

    #[test]
    fn goaway_round_trip() {
        let f = Frame::Goaway {
            last_stream_id: 7,
            error_code: H2ErrorCode::NoError as u32,
            debug_data: b"bye".to_vec(),
        };
        if let Frame::Goaway { last_stream_id, error_code, debug_data } = round_trip(f) {
            assert_eq!(last_stream_id, 7);
            assert_eq!(error_code, 0);
            assert_eq!(debug_data, b"bye");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn goaway_non_zero_stream_rejected() {
        let mut buf = Vec::new();
        encode_frame(&Frame::Goaway { last_stream_id: 0, error_code: 0, debug_data: vec![] }, &mut buf);
        // Set stream-id to 1.
        buf[8] = 1;
        let h = parse_frame_header(&buf).unwrap();
        let payload = &buf[9..];
        assert!(parse_frame(&h, payload).is_err());
    }

    // ── WINDOW_UPDATE ────────────────────────────────────────────────────

    #[test]
    fn window_update_round_trip() {
        let f = Frame::WindowUpdate { stream_id: 3, increment: 65535 };
        if let Frame::WindowUpdate { stream_id, increment } = round_trip(f) {
            assert_eq!(stream_id, 3);
            assert_eq!(increment, 65535);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn window_update_zero_increment_rejected() {
        let mut buf = Vec::new();
        write_header(&mut buf, 4, FrameType::WindowUpdate as u8, 0, 1);
        buf.extend_from_slice(&[0u8; 4]); // increment = 0
        let h = parse_frame_header(&buf).unwrap();
        assert!(parse_frame(&h, &[0u8; 4]).is_err());
    }

    // ── RST_STREAM ────────────────────────────────────────────────────────

    #[test]
    fn rst_stream_round_trip() {
        let f = Frame::RstStream { stream_id: 5, error_code: H2ErrorCode::Cancel as u32 };
        if let Frame::RstStream { stream_id, error_code } = round_trip(f) {
            assert_eq!(stream_id, 5);
            assert_eq!(error_code, 0x8);
        } else {
            panic!("wrong variant");
        }
    }

    // ── HEADERS ───────────────────────────────────────────────────────────

    #[test]
    fn headers_round_trip() {
        let block = b":method GET".to_vec();
        let f = Frame::Headers {
            stream_id: 1,
            end_stream: false,
            end_headers: true,
            header_block: block.clone(),
        };
        if let Frame::Headers { stream_id, end_headers, header_block, .. } = round_trip(f) {
            assert_eq!(stream_id, 1);
            assert!(end_headers);
            assert_eq!(header_block, block);
        } else {
            panic!("wrong variant");
        }
    }

    // ── PRIORITY ─────────────────────────────────────────────────────────

    #[test]
    fn priority_round_trip() {
        let f = Frame::Priority { stream_id: 2, exclusive: true, dependency: 1, weight: 15 };
        if let Frame::Priority { stream_id, exclusive, dependency, weight } = round_trip(f) {
            assert_eq!(stream_id, 2);
            assert!(exclusive);
            assert_eq!(dependency, 1);
            assert_eq!(weight, 15);
        } else {
            panic!("wrong variant");
        }
    }

    // ── CONTINUATION ─────────────────────────────────────────────────────

    #[test]
    fn continuation_round_trip() {
        let block = b"extra-headers".to_vec();
        let f = Frame::Continuation { stream_id: 1, end_headers: true, header_block: block.clone() };
        if let Frame::Continuation { end_headers, header_block, .. } = round_trip(f) {
            assert!(end_headers);
            assert_eq!(header_block, block);
        } else {
            panic!("wrong variant");
        }
    }

    // ── PUSH_PROMISE ─────────────────────────────────────────────────────

    #[test]
    fn push_promise_round_trip() {
        let block = b"pushed-headers".to_vec();
        let f = Frame::PushPromise { stream_id: 1, promised_id: 2, header_block: block.clone() };
        if let Frame::PushPromise { promised_id, header_block, .. } = round_trip(f) {
            assert_eq!(promised_id, 2);
            assert_eq!(header_block, block);
        } else {
            panic!("wrong variant");
        }
    }
}
