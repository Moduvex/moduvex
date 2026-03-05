//! Async I/O helpers for the HTTP/2 connection manager.
//!
//! Splits raw frame I/O out of `connection.rs` to keep each file under 200
//! lines. All functions operate on the [`super::stream::Stream`] transport
//! abstraction (plain TCP or TLS).

use std::pin::Pin;

use moduvex_runtime::net::{AsyncRead, AsyncWrite};

use super::error::{H2Error, H2ErrorCode};
use super::frame::{self, Frame};
use crate::server::tls::Stream;

// ── Frame reading ─────────────────────────────────────────────────────────────

/// Read exactly `n` bytes from `stream` into a fresh `Vec<u8>`.
pub(super) async fn read_exact(stream: &mut Stream, n: usize) -> Result<Vec<u8>, H2Error> {
    let mut buf = vec![0u8; n];
    let mut pos = 0;
    while pos < n {
        let chunk = poll_read(stream, &mut buf[pos..]).await.map_err(|e| {
            H2Error::connection(H2ErrorCode::InternalError, format!("io read: {e}"))
        })?;
        if chunk == 0 {
            return Err(H2Error::connection(
                H2ErrorCode::InternalError,
                "connection closed mid-frame",
            ));
        }
        pos += chunk;
    }
    Ok(buf)
}

/// Read a complete HTTP/2 frame (9-byte header + payload).
pub(super) async fn read_frame(stream: &mut Stream) -> Result<Frame, H2Error> {
    let header_bytes = read_exact(stream, 9).await?;
    let header = frame::parse_frame_header(&header_bytes)
        .ok_or_else(|| H2Error::connection(H2ErrorCode::InternalError, "header parse failed"))?;

    let payload = if header.length > 0 {
        read_exact(stream, header.length as usize).await?
    } else {
        Vec::new()
    };

    frame::parse_frame(&header, &payload)
}

/// Write an encoded frame to `stream`.
pub(super) async fn write_frame(stream: &mut Stream, frame: &Frame) -> Result<(), H2Error> {
    let mut buf = Vec::new();
    frame::encode_frame(frame, &mut buf);
    write_all(stream, &buf).await
}

/// Write raw bytes to `stream`, looping until all bytes are sent.
pub(super) async fn write_all(stream: &mut Stream, buf: &[u8]) -> Result<(), H2Error> {
    use std::future::poll_fn;
    let mut sent = 0;
    while sent < buf.len() {
        let n = poll_fn(|cx| Pin::new(&mut *stream).poll_write(cx, &buf[sent..]))
            .await
            .map_err(|e| H2Error::connection(H2ErrorCode::InternalError, format!("io write: {e}")))?;
        if n == 0 {
            return Err(H2Error::connection(
                H2ErrorCode::InternalError,
                "write returned 0 bytes",
            ));
        }
        sent += n;
    }
    Ok(())
}

// ── Internal poll helpers ─────────────────────────────────────────────────────

async fn poll_read(stream: &mut Stream, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::future::poll_fn;
    poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, buf)).await
}

// ── SETTINGS helpers ──────────────────────────────────────────────────────────

/// Apply incoming SETTINGS values into a [`super::connection::H2Settings`].
pub(super) fn apply_settings(
    settings: &mut super::connection::H2Settings,
    values: &[(u16, u32)],
) -> Result<(), H2Error> {
    use super::frame::{
        SETTINGS_ENABLE_PUSH, SETTINGS_HEADER_TABLE_SIZE, SETTINGS_INITIAL_WINDOW_SIZE,
        SETTINGS_MAX_CONCURRENT_STREAMS, SETTINGS_MAX_FRAME_SIZE, SETTINGS_MAX_HEADER_LIST_SIZE,
    };

    for &(id, val) in values {
        match id {
            SETTINGS_HEADER_TABLE_SIZE => settings.header_table_size = val,
            SETTINGS_ENABLE_PUSH => {
                if val > 1 {
                    return Err(H2Error::connection(
                        H2ErrorCode::ProtocolError,
                        "ENABLE_PUSH must be 0 or 1",
                    ));
                }
                settings.enable_push = val == 1;
            }
            SETTINGS_MAX_CONCURRENT_STREAMS => settings.max_concurrent_streams = val,
            SETTINGS_INITIAL_WINDOW_SIZE => {
                if val as i64 > (1 << 31) - 1 {
                    return Err(H2Error::connection(
                        H2ErrorCode::FlowControlError,
                        "INITIAL_WINDOW_SIZE too large",
                    ));
                }
                settings.initial_window_size = val;
            }
            SETTINGS_MAX_FRAME_SIZE => {
                if !(16_384..=16_777_215).contains(&val) {
                    return Err(H2Error::connection(
                        H2ErrorCode::ProtocolError,
                        "MAX_FRAME_SIZE out of range",
                    ));
                }
                settings.max_frame_size = val;
            }
            SETTINGS_MAX_HEADER_LIST_SIZE => settings.max_header_list_size = val,
            // Unknown SETTINGS identifiers MUST be ignored per RFC 9113 §6.5.
            _ => {}
        }
    }
    Ok(())
}
