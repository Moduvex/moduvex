//! PostgreSQL wire protocol message framing.
//!
//! **Frontend messages** (client → server):
//!   - Startup: 4-byte length + payload (no type byte)
//!   - All others: 1-byte type + 4-byte length + payload
//!
//! **Backend messages** (server → client):
//!   - All: 1-byte type + 4-byte length + payload
//!
//! The length field includes itself (4 bytes) but NOT the type byte.

use std::future::poll_fn;
use std::pin::Pin;

use moduvex_runtime::net::{AsyncRead, AsyncWrite};

use crate::error::{DbError, Result};

// ── Reading ───────────────────────────────────────────────────────────────────

/// Read exactly `n` bytes from `reader` into a new `Vec<u8>`.
///
/// Loops until all bytes are filled (handles short reads from non-blocking I/O).
pub async fn read_exact<R: AsyncRead + Unpin>(reader: &mut R, n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        let chunk = poll_fn(|cx| Pin::new(&mut *reader).poll_read(cx, &mut buf[filled..])).await?;
        if chunk == 0 {
            return Err(DbError::Protocol("unexpected EOF reading from server".into()));
        }
        filled += chunk;
    }
    Ok(buf)
}

/// Read one backend message frame: returns `(type_byte, payload)`.
///
/// Format: `[type: u8][length: i32 BE][payload: length-4 bytes]`
pub async fn read_backend_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<(u8, Vec<u8>)> {
    // 1-byte message type
    let hdr = read_exact(reader, 5).await?;
    let msg_type = hdr[0];
    let length = i32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
    if length < 4 {
        return Err(DbError::Protocol(format!(
            "invalid message length {length} for type 0x{msg_type:02X}"
        )));
    }
    // payload = length - 4 (length field includes itself)
    let payload_len = (length - 4) as usize;
    let payload = if payload_len > 0 {
        read_exact(reader, payload_len).await?
    } else {
        Vec::new()
    };
    Ok((msg_type, payload))
}

// ── Writing ───────────────────────────────────────────────────────────────────

/// Write all bytes to `writer`, looping on short writes.
pub async fn write_all<W: AsyncWrite + Unpin>(writer: &mut W, buf: &[u8]) -> Result<()> {
    let mut sent = 0;
    while sent < buf.len() {
        let n = poll_fn(|cx| Pin::new(&mut *writer).poll_write(cx, &buf[sent..])).await?;
        if n == 0 {
            return Err(DbError::Protocol("zero-byte write to server".into()));
        }
        sent += n;
    }
    Ok(())
}

/// Write a framed frontend message: `[type_byte][length i32 BE][payload]`.
///
/// `length` = 4 + payload.len() (length includes itself, not the type byte).
pub async fn write_frontend_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg_type: u8,
    payload: &[u8],
) -> Result<()> {
    let length = (4 + payload.len()) as i32;
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(msg_type);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    write_all(writer, &frame).await
}

/// Write the startup message (no type byte): `[length i32 BE][payload]`.
///
/// The startup message uses a special format without a leading type byte.
pub async fn write_startup_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> Result<()> {
    let length = (4 + payload.len()) as i32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    write_all(writer, &frame).await
}

// ── Payload helpers ───────────────────────────────────────────────────────────

/// Read a null-terminated C-string from `bytes` starting at `offset`.
///
/// Returns `(string, new_offset)`. The offset advances past the null byte.
pub fn read_cstring(bytes: &[u8], offset: usize) -> Result<(String, usize)> {
    let start = offset;
    let mut pos = offset;
    while pos < bytes.len() && bytes[pos] != 0 {
        pos += 1;
    }
    if pos >= bytes.len() {
        return Err(DbError::Protocol("unterminated C-string in message payload".into()));
    }
    let s = String::from_utf8(bytes[start..pos].to_vec())
        .map_err(|_| DbError::Protocol("non-UTF-8 C-string in payload".into()))?;
    Ok((s, pos + 1)) // skip null byte
}

/// Append a null-terminated C-string to `buf`.
pub fn write_cstring(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cstring_roundtrip() {
        let mut buf = Vec::new();
        write_cstring(&mut buf, "hello");
        write_cstring(&mut buf, "world");
        let (s1, off1) = read_cstring(&buf, 0).unwrap();
        let (s2, _off2) = read_cstring(&buf, off1).unwrap();
        assert_eq!(s1, "hello");
        assert_eq!(s2, "world");
    }

    #[test]
    fn cstring_unterminated_returns_err() {
        let buf = b"no_null".to_vec();
        assert!(read_cstring(&buf, 0).is_err());
    }

    #[test]
    fn frontend_message_frame_layout() {
        // Verify frame = [type][len i32 BE = 4+payload_len][payload]
        let payload = b"SELECT 1";
        let msg_type = b'Q';
        let expected_len = (4i32 + payload.len() as i32).to_be_bytes();

        let mut frame = Vec::new();
        frame.push(msg_type);
        frame.extend_from_slice(&expected_len);
        frame.extend_from_slice(payload);

        assert_eq!(frame[0], b'Q');
        assert_eq!(&frame[1..5], &expected_len);
        assert_eq!(&frame[5..], payload);
    }

    #[test]
    fn startup_message_has_no_type_byte() {
        // startup message = [len i32 BE][payload], no type byte
        let payload = b"\x00\x03\x00\x00user\x00postgres\x00\x00";
        let length = (4i32 + payload.len() as i32).to_be_bytes();
        let mut frame = Vec::new();
        frame.extend_from_slice(&length);
        frame.extend_from_slice(payload);
        // First 4 bytes are length, not a type byte
        assert_eq!(
            i32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]),
            4 + payload.len() as i32
        );
    }
}
