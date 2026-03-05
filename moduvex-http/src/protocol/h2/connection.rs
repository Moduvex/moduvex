//! HTTP/2 connection manager (RFC 9113).
//!
//! Owns the stream table, HPACK codec, settings, and flow-control state.
//! Async I/O helpers live in [`super::connection_io`] to keep this file under
//! 200 lines.
//!
//! # Lifecycle
//! 1. `H2Connection::new()` — allocate state.
//! 2. `handle_preface()` — exchange connection preface + SETTINGS.
//! 3. Loop: `read_frame()` → `process_frame()` → optionally `send_response()`.
//! 4. `send_goaway()` on shutdown or unrecoverable error.

use std::collections::HashMap;

use super::connection_io as io;
use super::error::{H2Error, H2ErrorCode};
use super::flow_control::{FlowController, DEFAULT_WINDOW_SIZE};
use super::frame::{self, Frame};
use super::hpack::{HpackDecoder, HpackEncoder};
use super::stream::{H2Stream, StreamState};
use crate::body::Body;
use crate::header::HeaderMap;
use crate::request::{HttpVersion, Request};
use crate::response::Response;
use crate::routing::method::Method;
use crate::server::tls::Stream;


/// HTTP/2 client connection preface magic (RFC 9113 §3.4).
pub const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

// ── Default constants ─────────────────────────────────────────────────────────

pub const DEFAULT_MAX_CONCURRENT_STREAMS: u32 = 100;
pub const DEFAULT_MAX_FRAME_SIZE: u32 = 16_384;
pub const DEFAULT_HEADER_TABLE_SIZE: u32 = 4_096;
pub const DEFAULT_MAX_HEADER_LIST_SIZE: u32 = 65_536;

// ── H2Settings ────────────────────────────────────────────────────────────────

/// Negotiated HTTP/2 settings for one side of the connection.
#[derive(Debug, Clone)]
pub struct H2Settings {
    pub header_table_size: u32,
    pub enable_push: bool,
    pub max_concurrent_streams: u32,
    pub initial_window_size: u32,
    pub max_frame_size: u32,
    pub max_header_list_size: u32,
}

impl Default for H2Settings {
    fn default() -> Self {
        Self {
            header_table_size: DEFAULT_HEADER_TABLE_SIZE,
            enable_push: true,
            max_concurrent_streams: DEFAULT_MAX_CONCURRENT_STREAMS,
            initial_window_size: DEFAULT_WINDOW_SIZE,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            max_header_list_size: DEFAULT_MAX_HEADER_LIST_SIZE,
        }
    }
}

// ── H2Connection ─────────────────────────────────────────────────────────────

/// HTTP/2 connection — drives the full request/response lifecycle.
pub struct H2Connection {
    /// Active streams keyed by stream ID.
    streams: HashMap<u32, H2Stream>,
    /// HPACK decoder for incoming header blocks.
    decoder: HpackDecoder,
    /// HPACK encoder for outgoing header blocks.
    encoder: HpackEncoder,
    /// Our own advertised settings.
    pub local_settings: H2Settings,
    /// Peer's negotiated settings (updated from SETTINGS frames).
    pub remote_settings: H2Settings,
    /// Connection-level flow-control windows.
    flow: FlowController,
    /// Highest stream ID seen from the client (for GOAWAY).
    last_stream_id: u32,
    /// True after we have emitted GOAWAY.
    pub goaway_sent: bool,
}

impl H2Connection {
    /// Allocate a fresh connection with RFC defaults.
    pub fn new() -> Self {
        let settings = H2Settings::default();
        Self {
            streams: HashMap::new(),
            decoder: HpackDecoder::new(DEFAULT_HEADER_TABLE_SIZE as usize),
            encoder: HpackEncoder::new(),
            flow: FlowController::new(settings.initial_window_size),
            local_settings: settings,
            remote_settings: H2Settings::default(),
            last_stream_id: 0,
            goaway_sent: false,
        }
    }

    /// Exchange the HTTP/2 connection preface.
    ///
    /// 1. Validate 24-byte client magic.
    /// 2. Read the client's initial SETTINGS frame.
    /// 3. Send our SETTINGS + SETTINGS ACK.
    pub async fn handle_preface(&mut self, stream: &mut Stream) -> Result<(), H2Error> {
        // 1. Client magic
        let magic = io::read_exact(stream, H2_PREFACE.len()).await?;
        if magic != H2_PREFACE {
            return Err(H2Error::connection(
                H2ErrorCode::ProtocolError,
                "invalid connection preface",
            ));
        }

        // 2. Client SETTINGS (may be empty)
        let client_settings_frame = io::read_frame(stream).await?;
        if let Frame::Settings { ack: false, ref values } = client_settings_frame {
            io::apply_settings(&mut self.remote_settings, values)?;
        } else {
            return Err(H2Error::connection(
                H2ErrorCode::ProtocolError,
                "expected SETTINGS after preface",
            ));
        }

        // 3. Send our SETTINGS
        let our_settings = Frame::Settings {
            ack: false,
            values: vec![
                (frame::SETTINGS_MAX_CONCURRENT_STREAMS, self.local_settings.max_concurrent_streams),
                (frame::SETTINGS_INITIAL_WINDOW_SIZE, self.local_settings.initial_window_size),
                (frame::SETTINGS_MAX_FRAME_SIZE, self.local_settings.max_frame_size),
            ],
        };
        io::write_frame(stream, &our_settings).await?;

        // 4. ACK client's SETTINGS
        io::write_frame(stream, &Frame::Settings { ack: true, values: vec![] }).await
    }

    /// Read one frame from the wire (delegates to I/O helper).
    pub async fn read_frame(&mut self, stream: &mut Stream) -> Result<Frame, H2Error> {
        io::read_frame(stream).await
    }

    /// Write one frame to the wire (delegates to I/O helper).
    pub async fn write_frame(&self, stream: &mut Stream, frame: &Frame) -> Result<(), H2Error> {
        io::write_frame(stream, frame).await
    }

    /// Process an incoming frame, returning a complete `Request` when one is ready.
    ///
    /// Returns `Ok(Some((stream_id, request)))` when a stream has received all
    /// headers (and possibly body). Returns `Ok(None)` for control frames or
    /// incomplete streams.
    pub fn process_frame(&mut self, frame: Frame) -> Result<Option<(u32, Request)>, H2Error> {
        match frame {
            Frame::Settings { ack: false, ref values } => {
                io::apply_settings(&mut self.remote_settings, values)?;
                // Caller must send SETTINGS ACK via write_frame.
                Ok(None)
            }
            Frame::Settings { ack: true, .. } => Ok(None),

            Frame::Headers { stream_id, end_stream, end_headers, header_block } => {
                self.last_stream_id = self.last_stream_id.max(stream_id);
                let win = self.remote_settings.initial_window_size;
                let s = self.streams.entry(stream_id).or_insert_with(|| H2Stream::new(stream_id, win));
                s.recv_headers()?;
                let decoded = self.decoder.decode(&header_block)?;
                s.headers.extend(decoded);
                if end_headers {
                    s.headers_complete = true;
                }
                if end_stream {
                    s.recv_end_stream()?;
                }
                if s.headers_complete
                    && matches!(s.state, StreamState::HalfClosedRemote | StreamState::Open)
                    && end_stream
                {
                    return Ok(Some((stream_id, build_request(&self.streams[&stream_id]))));
                }
                Ok(None)
            }

            Frame::Continuation { stream_id, end_headers, header_block } => {
                let s = self.streams.get_mut(&stream_id).ok_or_else(|| {
                    H2Error::connection(H2ErrorCode::ProtocolError, "CONTINUATION on unknown stream")
                })?;
                let decoded = self.decoder.decode(&header_block)?;
                s.headers.extend(decoded);
                if end_headers {
                    s.headers_complete = true;
                }
                Ok(None)
            }

            Frame::Data { stream_id, end_stream, payload } => {
                let len = payload.len() as u32;
                self.flow.consume_recv(len)?;
                let s = self.streams.get_mut(&stream_id).ok_or_else(|| {
                    H2Error::stream(stream_id, H2ErrorCode::StreamClosed, "DATA on unknown stream")
                })?;
                s.consume_recv_window(len)?;
                s.body.extend_from_slice(&payload);
                if end_stream {
                    s.recv_end_stream()?;
                    return Ok(Some((stream_id, build_request(&self.streams[&stream_id]))));
                }
                Ok(None)
            }

            Frame::WindowUpdate { stream_id, increment } => {
                if stream_id == 0 {
                    self.flow.window_update(increment)?;
                } else if let Some(s) = self.streams.get_mut(&stream_id) {
                    s.add_send_window(increment)?;
                }
                Ok(None)
            }

            Frame::Ping { ack: false, data } => {
                // Caller must echo PING ACK; signal via None — caller checks queued frames.
                // Store the ping data so the caller can send the ACK.
                // We return it encoded as a synthetic frame via a side channel would be ideal,
                // but keeping it simple: caller should detect PING non-ACK separately.
                // For now we queue the ACK by returning None — the outer loop handles it.
                let _ = data; // ACK is sent by the connection loop, not here
                Ok(None)
            }
            Frame::Ping { ack: true, .. } => Ok(None),

            Frame::Goaway { .. } => {
                // Peer is shutting down — mark for close.
                self.goaway_sent = true;
                Ok(None)
            }

            Frame::RstStream { stream_id, .. } => {
                if let Some(s) = self.streams.get_mut(&stream_id) {
                    s.reset();
                }
                Ok(None)
            }

            Frame::PushPromise { stream_id, .. } => {
                // Clients must not send PUSH_PROMISE; treat as protocol error.
                Err(H2Error::stream(
                    stream_id,
                    H2ErrorCode::ProtocolError,
                    "clients may not send PUSH_PROMISE",
                ))
            }

            // PRIORITY frames are advisory; safe to ignore.
            Frame::Priority { .. } => Ok(None),
        }
    }

    /// Encode and send an HTTP response for `stream_id`.
    ///
    /// Sends a HEADERS frame (with END_HEADERS set), then DATA frame(s) if the
    /// response has a body. Respects `max_frame_size` and flow-control windows.
    pub async fn send_response(
        &mut self,
        stream_id: u32,
        response: Response,
        stream: &mut Stream,
    ) -> Result<(), H2Error> {
        let status_str = response.status.as_u16().to_string();
        let mut header_pairs: Vec<(&[u8], &[u8])> = vec![(b":status", status_str.as_bytes())];

        // Collect header name/value pairs from the response.
        let raw_headers: Vec<(Vec<u8>, Vec<u8>)> = response
            .headers
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.to_vec()))
            .collect();
        for (k, v) in &raw_headers {
            header_pairs.push((k.as_slice(), v.as_slice()));
        }

        let mut header_block = Vec::new();
        self.encoder.encode(&header_pairs, &mut header_block);

        let body_bytes = response.body.into_bytes();
        let has_body = !body_bytes.is_empty();

        // HEADERS frame — END_STREAM when there is no body.
        let headers_frame = Frame::Headers {
            stream_id,
            end_stream: !has_body,
            end_headers: true,
            header_block,
        };
        io::write_frame(stream, &headers_frame).await?;

        // Transition stream state.
        if let Some(s) = self.streams.get_mut(&stream_id) {
            if !has_body {
                let _ = s.send_end_stream();
            }
        }

        if has_body {
            self.send_data(stream_id, &body_bytes, stream).await?;
        }
        Ok(())
    }

    /// Send DATA frame(s), chunking at `max_frame_size` and honouring flow control.
    async fn send_data(
        &mut self,
        stream_id: u32,
        data: &[u8],
        stream: &mut Stream,
    ) -> Result<(), H2Error> {
        let max_chunk = self.local_settings.max_frame_size as usize;
        let mut offset = 0;

        while offset < data.len() {
            let end = (offset + max_chunk).min(data.len());
            let chunk = &data[offset..end];
            let end_stream = end == data.len();

            // Enforce connection-level flow control.
            self.flow.consume_send(chunk.len() as u32)?;

            // Enforce stream-level flow control.
            if let Some(s) = self.streams.get_mut(&stream_id) {
                s.consume_send_window(chunk.len() as u32)?;
            }

            let data_frame = Frame::Data {
                stream_id,
                end_stream,
                payload: chunk.to_vec(),
            };
            io::write_frame(stream, &data_frame).await?;

            if end_stream {
                if let Some(s) = self.streams.get_mut(&stream_id) {
                    let _ = s.send_end_stream();
                }
            }
            offset = end;
        }
        Ok(())
    }

    /// Send a GOAWAY frame and mark the connection for shutdown.
    pub async fn send_goaway(
        &mut self,
        stream: &mut Stream,
        error_code: H2ErrorCode,
    ) -> Result<(), H2Error> {
        let frame = Frame::Goaway {
            last_stream_id: self.last_stream_id,
            error_code: error_code as u32,
            debug_data: vec![],
        };
        io::write_frame(stream, &frame).await?;
        self.goaway_sent = true;
        Ok(())
    }
}

impl Default for H2Connection {
    fn default() -> Self {
        Self::new()
    }
}

// ── Request builder ───────────────────────────────────────────────────────────

/// Build a [`Request`] from the accumulated state on a completed stream.
fn build_request(s: &H2Stream) -> Request {
    let mut method = Method::GET;
    let mut path = String::from("/");
    let mut query: Option<String> = None;
    let mut header_map = HeaderMap::new();

    for (name, value) in &s.headers {
        match name.as_slice() {
            b":method" => {
                method = Method::from_bytes(value).unwrap_or(Method::GET);
            }
            b":path" => {
                let full = std::str::from_utf8(value).unwrap_or("/");
                if let Some(pos) = full.find('?') {
                    path = full[..pos].to_string();
                    let q = &full[pos + 1..];
                    if !q.is_empty() {
                        query = Some(q.to_string());
                    }
                } else {
                    path = full.to_string();
                }
            }
            b":authority" => {
                header_map.insert("host", value.clone());
            }
            // Skip other pseudo-headers (:scheme, :status).
            name if name.starts_with(b":") => {}
            _ => {
                let key = std::str::from_utf8(name).unwrap_or("").to_string();
                header_map.insert(key, value.clone());
            }
        }
    }

    let body = if s.body.is_empty() {
        Body::Empty
    } else {
        Body::Fixed(s.body.clone())
    };

    Request {
        method,
        path,
        query,
        version: HttpVersion::Http2,
        headers: header_map,
        body,
        peer_addr: None,
        extensions: crate::request::Extensions::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::h2::stream::H2Stream;

    #[test]
    fn default_settings_are_rfc_values() {
        let c = H2Connection::new();
        assert_eq!(c.local_settings.max_frame_size, DEFAULT_MAX_FRAME_SIZE);
        assert_eq!(c.local_settings.initial_window_size, DEFAULT_WINDOW_SIZE);
        assert_eq!(c.local_settings.max_concurrent_streams, DEFAULT_MAX_CONCURRENT_STREAMS);
    }

    #[test]
    fn build_request_parses_pseudo_headers() {
        let mut s = H2Stream::new(1, 65_535);
        s.headers = vec![
            (b":method".to_vec(), b"POST".to_vec()),
            (b":path".to_vec(), b"/api/v1?foo=bar".to_vec()),
            (b":authority".to_vec(), b"example.com".to_vec()),
            (b"content-type".to_vec(), b"application/json".to_vec()),
        ];
        s.body = b"{}".to_vec();

        let req = build_request(&s);
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path, "/api/v1");
        assert_eq!(req.query, Some("foo=bar".to_string()));
        assert_eq!(req.header("host"), Some("example.com"));
        assert_eq!(req.header("content-type"), Some("application/json"));
        assert_eq!(req.body.into_bytes(), b"{}");
    }

    #[test]
    fn build_request_empty_body_becomes_body_empty() {
        let mut s = H2Stream::new(1, 65_535);
        s.headers = vec![
            (b":method".to_vec(), b"GET".to_vec()),
            (b":path".to_vec(), b"/".to_vec()),
        ];
        let req = build_request(&s);
        assert!(matches!(req.body, Body::Empty));
    }

    #[test]
    fn build_request_path_without_query() {
        let mut s = H2Stream::new(1, 65_535);
        s.headers = vec![
            (b":method".to_vec(), b"GET".to_vec()),
            (b":path".to_vec(), b"/users/42".to_vec()),
        ];
        let req = build_request(&s);
        assert_eq!(req.path, "/users/42");
        assert!(req.query.is_none());
    }

    #[test]
    fn goaway_sent_flag_starts_false() {
        let c = H2Connection::new();
        assert!(!c.goaway_sent);
    }
}
