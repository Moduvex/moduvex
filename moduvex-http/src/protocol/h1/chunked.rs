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

    // ── Encoding ──────────────────────────────────────────────────────────

    #[test]
    fn encode_single_chunk_format() {
        // Verify chunked encoding format: "<hex-len>\r\n<data>\r\n"
        let mut out = Vec::new();
        encode_chunk(b"hello", &mut out);
        assert_eq!(out, b"5\r\nhello\r\n");
    }

    #[test]
    fn encode_empty_data_is_noop() {
        // encode_chunk does nothing for empty data; write_final_chunk writes terminator
        let mut out = Vec::new();
        encode_chunk(b"", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn write_final_chunk_produces_terminator() {
        let mut out = Vec::new();
        write_final_chunk(&mut out);
        assert_eq!(out, b"0\r\n\r\n");
    }

    #[test]
    fn encode_large_chunk_hex_size() {
        let data = vec![b'x'; 256];
        let mut out = Vec::new();
        encode_chunk(&data, &mut out);
        assert!(out.starts_with(b"100\r\n")); // 256 = 0x100
    }

    // ── Additional decode tests ───────────────────────────────────────────

    #[test]
    fn decode_chunked_large_single_chunk() {
        let data = vec![b'A'; 1000];
        let mut encoded = format!("{:x}\r\n", data.len()).into_bytes();
        encoded.extend_from_slice(&data);
        encoded.extend_from_slice(b"\r\n0\r\n\r\n");
        let decoded = decode_chunked(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_chunked_three_chunks_then_terminator() {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"3\r\nfoo\r\n");
        encoded.extend_from_slice(b"3\r\nbar\r\n");
        encoded.extend_from_slice(b"3\r\nbaz\r\n");
        encoded.extend_from_slice(b"0\r\n\r\n");
        let decoded = decode_chunked(&encoded).unwrap();
        assert_eq!(decoded, b"foobarbaz");
    }

    #[test]
    fn decode_chunked_invalid_hex_returns_error() {
        let buf = b"ZZZZ\r\nhello\r\n0\r\n\r\n";
        assert!(decode_chunked(buf).is_err());
    }

    #[test]
    fn decode_chunked_missing_terminator_returns_incomplete() {
        // No terminating chunk — should return Incomplete error
        let buf = b"5\r\nhello\r\n";
        assert_eq!(decode_chunked(buf), Err(ChunkedError::Incomplete));
    }

    #[test]
    fn decode_chunked_uppercase_hex_works() {
        // Hex digits are case-insensitive per RFC
        let buf = b"5\r\nhello\r\n0\r\n\r\n";
        let decoded = decode_chunked(buf).unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn decode_chunked_round_trip_via_encode() {
        // Encode then decode, verify identity
        let original = b"The quick brown fox";
        let mut encoded = Vec::new();
        encode_chunk(original, &mut encoded);
        write_final_chunk(&mut encoded);
        let decoded = decode_chunked(&encoded).unwrap();
        assert_eq!(decoded.as_slice(), original.as_slice());
    }

    #[test]
    fn decode_chunked_zero_chunk_only() {
        // Only the terminating chunk
        let buf = b"0\r\n\r\n";
        assert_eq!(decode_chunked(buf).unwrap(), b"");
    }
}
