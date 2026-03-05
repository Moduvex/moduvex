//! HPACK decoder (RFC 7541 Section 6).
//!
//! Parses a header block fragment into a list of (name, value) byte pairs,
//! updating the dynamic table as required by the spec.

use super::{huffman::huffman_decode, table::DynamicTable};
use crate::protocol::h2::error::{H2Error, H2ErrorCode};

/// A decoded header list: ordered `(name, value)` byte-vector pairs.
pub type HeaderList = Vec<(Vec<u8>, Vec<u8>)>;

// ── Public API ────────────────────────────────────────────────────────────────

/// HPACK decoder — maintains a dynamic table across calls within one connection.
pub struct HpackDecoder {
    table: DynamicTable,
}

impl HpackDecoder {
    /// Create a decoder with the given initial dynamic-table size limit (octets).
    pub fn new(max_table_size: usize) -> Self {
        Self { table: DynamicTable::new(max_table_size) }
    }

    /// Decode a complete header block fragment.
    ///
    /// Returns the ordered list of `(name, value)` pairs or a
    /// `CompressionError` on malformed input.
    pub fn decode(&mut self, block: &[u8]) -> Result<HeaderList, H2Error> {
        let mut headers = Vec::new();
        let mut pos = 0;

        while pos < block.len() {
            let first = block[pos];

            if first & 0x80 != 0 {
                // ── Indexed Header Field (RFC 7541 §6.1) ─────────────
                let (idx, consumed) = decode_integer(&block[pos..], 7)?;
                pos += consumed;
                if idx == 0 {
                    return Err(compression_err("indexed header: index 0 is invalid"));
                }
                let (n, v) = self
                    .table
                    .get(idx as usize)
                    .ok_or_else(|| compression_err("indexed header: index out of bounds"))?;
                headers.push((n.to_vec(), v.to_vec()));
            } else if first & 0xc0 == 0x40 {
                // ── Literal with Incremental Indexing (RFC 7541 §6.2.1) ──
                let (name, value, consumed) = self.decode_literal(&block[pos..], 6)?;
                pos += consumed;
                self.table.insert(name.clone(), value.clone());
                headers.push((name, value));
            } else if first & 0xe0 == 0x20 {
                // ── Dynamic Table Size Update (RFC 7541 §6.3) ────────
                let (new_size, consumed) = decode_integer(&block[pos..], 5)?;
                pos += consumed;
                self.table.resize(new_size as usize);
            } else {
                // ── Literal without Indexing / Never Indexed (§6.2.2 / §6.2.3) ──
                // 0x00 prefix (without indexing) and 0x10 (never indexed) — both 4-bit index.
                let (name, value, consumed) = self.decode_literal(&block[pos..], 4)?;
                pos += consumed;
                headers.push((name, value));
            }
        }

        Ok(headers)
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Decode one literal header field (name + value).
    /// `prefix_bits`: bit width of the name index in the first byte.
    /// Returns `(name, value, bytes_consumed)`.
    fn decode_literal(
        &self,
        buf: &[u8],
        prefix_bits: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, usize), H2Error> {
        let mut pos = 0;
        let (name_idx, consumed) = decode_integer(buf, prefix_bits)?;
        pos += consumed;

        let name = if name_idx == 0 {
            // Literal name string follows.
            let (s, n) = decode_string(&buf[pos..])?;
            pos += n;
            s
        } else {
            // Name comes from static or dynamic table.
            let (n, _) = self
                .table
                .get(name_idx as usize)
                .ok_or_else(|| compression_err("literal: name index out of bounds"))?;
            n.to_vec()
        };

        let (value, n) = decode_string(&buf[pos..])?;
        pos += n;

        Ok((name, value, pos))
    }
}

impl Default for HpackDecoder {
    fn default() -> Self {
        Self::new(4096)
    }
}

// ── Integer decoding (RFC 7541 §5.1) ─────────────────────────────────────────

/// Decode a variable-length integer with the given prefix bit count.
/// Returns `(value, bytes_consumed)`.
pub(super) fn decode_integer(buf: &[u8], prefix_bits: u8) -> Result<(u64, usize), H2Error> {
    if buf.is_empty() {
        return Err(compression_err("integer: buffer empty"));
    }
    let mask = (1u8 << prefix_bits) - 1;
    let prefix_val = (buf[0] & mask) as u64;

    if prefix_val < mask as u64 {
        return Ok((prefix_val, 1));
    }

    // Multi-byte continuation (MSB=1 means more bytes follow).
    let mut value = prefix_val;
    let mut shift = 0u32;
    let mut pos = 1;

    loop {
        if pos >= buf.len() {
            return Err(compression_err("integer: truncated"));
        }
        let byte = buf[pos];
        pos += 1;
        value += ((byte & 0x7f) as u64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if shift > 63 {
            return Err(compression_err("integer: overflow"));
        }
    }

    Ok((value, pos))
}

// ── String decoding (RFC 7541 §5.2) ──────────────────────────────────────────

/// Decode a length-prefixed string (optionally Huffman-encoded).
/// Returns `(bytes, bytes_consumed)`.
pub(super) fn decode_string(buf: &[u8]) -> Result<(Vec<u8>, usize), H2Error> {
    if buf.is_empty() {
        return Err(compression_err("string: buffer empty"));
    }
    let is_huffman = buf[0] & 0x80 != 0;
    let (length, header_len) = decode_integer(buf, 7)?;
    let length = length as usize;
    let end = header_len + length;

    if end > buf.len() {
        return Err(compression_err("string: truncated"));
    }
    let raw = &buf[header_len..end];

    let result = if is_huffman { huffman_decode(raw)? } else { raw.to_vec() };

    Ok((result, end))
}

// ── Error helper ──────────────────────────────────────────────────────────────

fn compression_err(msg: &'static str) -> H2Error {
    H2Error::connection(H2ErrorCode::CompressionError, msg)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_integer_one_byte() {
        // prefix_bits=5, value=10 (fits within prefix — no continuation)
        let (val, consumed) = decode_integer(&[0b00001010], 5).unwrap();
        assert_eq!(val, 10);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn decode_integer_multi_byte() {
        // RFC 7541 §C.1.1: prefix_bits=5, value=1337
        let buf = &[0x1f, 0x9a, 0x0a];
        let (val, consumed) = decode_integer(buf, 5).unwrap();
        assert_eq!(val, 1337);
        assert_eq!(consumed, 3);
    }

    #[test]
    fn decode_indexed_get_method() {
        // 0x82 = 1000_0010 → indexed representation, index=2 (:method GET)
        let mut dec = HpackDecoder::new(4096);
        let headers = dec.decode(&[0x82]).unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, b":method");
        assert_eq!(headers[0].1, b"GET");
    }

    #[test]
    fn decode_indexed_status_200() {
        // index=8 → :status 200
        let mut dec = HpackDecoder::new(4096);
        let headers = dec.decode(&[0x88]).unwrap();
        assert_eq!(headers[0].0, b":status");
        assert_eq!(headers[0].1, b"200");
    }

    #[test]
    fn decode_literal_without_indexing_literal_name() {
        // 0x00 = literal no index, name index=0 → literal name follows
        let mut block = vec![0x00];
        block.push(0x06); // name length 6 (no Huffman)
        block.extend_from_slice(b"x-test");
        block.push(0x02); // value length 2
        block.extend_from_slice(b"ok");

        let mut dec = HpackDecoder::new(4096);
        let headers = dec.decode(&block).unwrap();
        assert_eq!(headers[0].0, b"x-test");
        assert_eq!(headers[0].1, b"ok");
    }

    #[test]
    fn decode_literal_with_incremental_indexing_adds_to_table() {
        // 0x40 = literal incremental indexing, name index=0 → literal name
        let mut block = vec![0x40];
        block.push(0x04); // name length 4
        block.extend_from_slice(b"link");
        block.push(0x03); // value length 3
        block.extend_from_slice(b"rel");

        let mut dec = HpackDecoder::new(4096);
        let headers = dec.decode(&block).unwrap();
        assert_eq!(headers[0].0, b"link");
        assert_eq!(headers[0].1, b"rel");

        // Dynamic table must contain the entry at combined index 62.
        let (n, v) = dec.table.get(62).unwrap();
        assert_eq!(n, b"link");
        assert_eq!(v, b"rel");
    }

    #[test]
    fn invalid_index_zero_returns_error() {
        // 0x80 = indexed, index=0 — explicitly forbidden by RFC 7541 §6.1
        let mut dec = HpackDecoder::new(4096);
        assert!(dec.decode(&[0x80]).is_err());
    }

    #[test]
    fn decode_string_literal_no_huffman() {
        // MSB=0 → plain literal, length=5
        let buf = &[0x05, b'h', b'e', b'l', b'l', b'o'];
        let (s, consumed) = decode_string(buf).unwrap();
        assert_eq!(s, b"hello");
        assert_eq!(consumed, 6);
    }

    #[test]
    fn dynamic_table_size_update_zero() {
        // 0x20 = 001_00000 → size update, value=0 (evicts all dynamic entries)
        let mut dec = HpackDecoder::new(4096);
        assert!(dec.decode(&[0x20]).is_ok());
    }

    #[test]
    fn multiple_headers_in_one_block() {
        // Two indexed entries: :method GET (2) + :scheme https (7)
        let mut dec = HpackDecoder::new(4096);
        let headers = dec.decode(&[0x82, 0x87]).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[1].0, b":scheme");
        assert_eq!(headers[1].1, b"https");
    }
}
