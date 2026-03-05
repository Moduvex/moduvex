//! WebSocket stream — send/recv API with RFC 6455 §5.4 fragment reassembly.

use std::pin::Pin;

use crate::server::tls::Stream;

use super::frame::{decode_frame, encode_frame, Frame, FrameError, Opcode};
use super::{Message, WsError};

/// Maximum reassembly buffer size (16 MiB). Fragments exceeding this
/// limit trigger a protocol error to prevent memory exhaustion.
pub(crate) const MAX_FRAGMENT_SIZE: usize = 16 * 1024 * 1024;

/// An established WebSocket connection providing `send` / `recv` message API.
///
/// Constructed after the HTTP upgrade handshake completes. The underlying
/// `Stream` is consumed from the `Connection` and taken over by `WsStream`.
///
/// Fragmented messages (RFC 6455 §5.4) are transparently reassembled:
/// continuation frames are buffered until the final fragment arrives,
/// then emitted as a single [`Message`].
pub struct WsStream {
    stream: Stream,
    read_buf: Vec<u8>,
    closed: bool,
    /// Opcode of the in-progress fragmented message (`None` when idle).
    fragment_opcode: Option<Opcode>,
    /// Accumulation buffer for fragmented payloads.
    fragment_buf: Vec<u8>,
}

impl WsStream {
    pub(crate) fn new(stream: Stream) -> Self {
        Self {
            stream,
            read_buf: Vec::with_capacity(4096),
            closed: false,
            fragment_opcode: None,
            fragment_buf: Vec::new(),
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
            Message::Text(s)   => Frame::text(s.into_bytes()),
            Message::Binary(b) => Frame::binary(b),
            Message::Ping(d)   => Frame::ping(d),
            Message::Pong(d)   => Frame::pong(d),
            Message::Close     => {
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
            match decode_frame(&self.read_buf) {
                Ok((frame, consumed)) => {
                    self.read_buf.drain(..consumed);
                    match self.handle_frame(frame).await? {
                        Some(msg) => return Ok(msg),
                        None      => continue,
                    }
                }
                Err(FrameError::Incomplete) => {
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

    /// Process a decoded frame implementing RFC 6455 §5.4 fragmentation.
    async fn handle_frame(&mut self, frame: Frame) -> Result<Option<Message>, WsError> {
        if frame.opcode.is_control() {
            return self.handle_control(frame).await;
        }

        match (frame.fin, frame.opcode) {
            // Single unfragmented frame
            (true, Opcode::Text | Opcode::Binary) if self.fragment_opcode.is_none() => {
                self.finish_data_frame(frame.opcode, frame.payload)
            }
            // New data frame while fragment in progress
            (_, Opcode::Text | Opcode::Binary) if self.fragment_opcode.is_some() => {
                Err(WsError::Protocol(
                    "new data frame received during fragmented message".to_string(),
                ))
            }
            // Start fragment (FIN=0, Text/Binary)
            (false, Opcode::Text | Opcode::Binary) => {
                self.fragment_opcode = Some(frame.opcode);
                self.fragment_buf = frame.payload;
                self.check_fragment_size()?;
                Ok(None)
            }
            // Continue fragment (FIN=0, Continuation)
            (false, Opcode::Continuation) if self.fragment_opcode.is_some() => {
                self.fragment_buf.extend_from_slice(&frame.payload);
                self.check_fragment_size()?;
                Ok(None)
            }
            // Complete fragment (FIN=1, Continuation)
            (true, Opcode::Continuation) if self.fragment_opcode.is_some() => {
                self.fragment_buf.extend_from_slice(&frame.payload);
                self.check_fragment_size()?;
                let opcode = self.fragment_opcode.take().unwrap();
                let payload = std::mem::take(&mut self.fragment_buf);
                self.finish_data_frame(opcode, payload)
            }
            // Continuation without active fragment
            (_, Opcode::Continuation) => Err(WsError::Protocol(
                "continuation frame without preceding data frame".to_string(),
            )),
            _ => Err(WsError::Protocol("unexpected frame state".to_string())),
        }
    }

    async fn handle_control(&mut self, frame: Frame) -> Result<Option<Message>, WsError> {
        match frame.opcode {
            Opcode::Ping => {
                let pong = Frame::pong(frame.payload.clone());
                let mut buf = Vec::new();
                encode_frame(&pong, &mut buf);
                let _ = self.write_all(&buf).await;
                Ok(Some(Message::Ping(frame.payload)))
            }
            Opcode::Pong => Ok(Some(Message::Pong(frame.payload))),
            Opcode::Close => {
                if !self.closed {
                    self.closed = true;
                    let close = Frame::close(1000, b"");
                    let mut buf = Vec::new();
                    encode_frame(&close, &mut buf);
                    let _ = self.write_all(&buf).await;
                }
                Ok(Some(Message::Close))
            }
            _ => unreachable!("handle_control called with non-control opcode"),
        }
    }

    fn finish_data_frame(
        &self,
        opcode: Opcode,
        payload: Vec<u8>,
    ) -> Result<Option<Message>, WsError> {
        match opcode {
            Opcode::Text => {
                let s = String::from_utf8(payload)
                    .map_err(|e| WsError::Protocol(format!("invalid UTF-8: {e}")))?;
                Ok(Some(Message::Text(s)))
            }
            Opcode::Binary => Ok(Some(Message::Binary(payload))),
            _ => Err(WsError::Protocol("unexpected opcode in data frame".to_string())),
        }
    }

    fn check_fragment_size(&self) -> Result<(), WsError> {
        if self.fragment_buf.len() > MAX_FRAGMENT_SIZE {
            return Err(WsError::Protocol(format!(
                "fragmented message exceeds {} byte limit",
                MAX_FRAGMENT_SIZE
            )));
        }
        Ok(())
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
