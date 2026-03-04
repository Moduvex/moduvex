//! Chunked Transfer-Encoding decoder.
//!
//! Parses `chunk-size CRLF chunk-data CRLF` sequences from a byte buffer.
//! Returns fully assembled body bytes or an error on malformed input.
//!
//! Per RFC 9112 §7.1 — this decoder:
//! - Rejects non-hex chunk sizes
//! - Rejects oversized chunks (> 16 MB by default)
//! - Accepts the zero-length terminator with optional trailers (trailers ignored)
//! - Closes connection on any malformed input

/// Maximum individual chunk size (16 MB).
const MAX_CHUNK_SIZE: usize = 16 * 1024 * 1024;

/// Error during chunked decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkedError {
    /// Chunk size line contains invalid hex.
    BadChunkSize,
    /// Chunk exceeds the maximum allowed size.
    ChunkTooLarge,
    /// Chunk data is not terminated by CRLF.
    BadChunkTerminator,
    /// Buffer ended before the full chunk arrived.
    Incomplete,
}

impl std::fmt::Display for ChunkedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Decode a complete chunked-encoded body from `buf`.
///
/// `buf` must contain the full chunked body (including the `0\r\n\r\n`
/// terminator). Returns the assembled plaintext bytes on success.
pub fn decode_chunked(buf: &[u8]) -> Result<Vec<u8>, ChunkedError> {
    let mut pos = 0;
    let mut body = Vec::new();

    loop {
        // Find the CRLF that ends the chunk-size line.
        let crlf = find_crlf(&buf[pos..]).ok_or(ChunkedError::Incomplete)?;
        let size_line = &buf[pos..pos + crlf];
        // Chunk extensions (after ';') are ignored per RFC 9112 §7.1.1.
        let size_str = size_line.split(|&b| b == b';').next().unwrap_or(size_line);
        let size_str = std::str::from_utf8(size_str)
            .map_err(|_| ChunkedError::BadChunkSize)?
            .trim();
        let chunk_size =
            usize::from_str_radix(size_str, 16).map_err(|_| ChunkedError::BadChunkSize)?;
        pos += crlf + 2; // skip size line + CRLF

        if chunk_size == 0 {
            // Terminating chunk — skip optional trailers (ends at \r\n\r\n or \r\n).
            break;
        }

        if chunk_size > MAX_CHUNK_SIZE {
            return Err(ChunkedError::ChunkTooLarge);
        }

        // Ensure chunk data + trailing CRLF are present.
        if pos + chunk_size + 2 > buf.len() {
            return Err(ChunkedError::Incomplete);
        }

        body.extend_from_slice(&buf[pos..pos + chunk_size]);
        pos += chunk_size;

        // Expect CRLF after chunk data.
        if &buf[pos..pos + 2] != b"\r\n" {
            return Err(ChunkedError::BadChunkTerminator);
        }
        pos += 2;
    }

    Ok(body)
}

/// Encode `data` as a single chunked-TE chunk into `out`.
///
/// Appends `<hex-size>\r\n<data>\r\n`. Call `write_final_chunk(out)` when done.
pub fn encode_chunk(data: &[u8], out: &mut Vec<u8>) {
    if data.is_empty() {
        return;
    }
    let hex = format!("{:x}", data.len());
    out.extend_from_slice(hex.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
}

/// Append the chunked-TE terminating `0\r\n\r\n` chunk to `out`.
pub fn write_final_chunk(out: &mut Vec<u8>) {
    out.extend_from_slice(b"0\r\n\r\n");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Find the offset of `\r\n` within `buf`, returning the offset before it.
fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_single_chunk() {
        let buf = b"5\r\nhello\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(buf).unwrap(), b"hello");
    }

    #[test]
    fn decode_multi_chunk() {
        let buf = b"3\r\nfoo\r\n4\r\nbarr\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(buf).unwrap(), b"foobarr");
    }

    #[test]
    fn decode_empty_body() {
        let buf = b"0\r\n\r\n";
        assert_eq!(decode_chunked(buf).unwrap(), b"");
    }

    #[test]
    fn bad_hex_size() {
        let buf = b"xyz\r\nhello\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(buf), Err(ChunkedError::BadChunkSize));
    }

    #[test]
    fn missing_chunk_terminator() {
        let buf = b"5\r\nhello NOCRLF0\r\n\r\n";
        // chunk_size=5, data="hello", but next 2 bytes are " N" not "\r\n"
        assert_eq!(decode_chunked(buf), Err(ChunkedError::BadChunkTerminator));
    }

    #[test]
    fn incomplete_data() {
        let buf = b"5\r\nhel"; // only 3 of 5 bytes
        assert_eq!(decode_chunked(buf), Err(ChunkedError::Incomplete));
    }

    #[test]
    fn encode_and_decode_roundtrip() {
        let mut out = Vec::new();
        encode_chunk(b"hello", &mut out);
        encode_chunk(b" world", &mut out);
        write_final_chunk(&mut out);
        assert_eq!(decode_chunked(&out).unwrap(), b"hello world");
    }

    #[test]
    fn chunk_extension_ignored() {
        let buf = b"5;ext=ignored\r\nhello\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(buf).unwrap(), b"hello");
    }
}
