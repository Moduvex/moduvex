//! HPACK encoder (RFC 7541 Section 6).
//!
//! Strategy (KISS):
//! - Static table exact match (name+value) → indexed header field.
//! - Static table name-only match → literal with indexed name, no dynamic indexing.
//! - No match → literal with literal name, no dynamic indexing.
//! - No Huffman encoding on output (valid per spec; decoding is mandatory, encoding is optional).
//! - No dynamic table on the encode side (avoids state complexity).

use super::table::STATIC_TABLE;

// ── Public API ────────────────────────────────────────────────────────────────

/// HPACK header block encoder (RFC 7541).
///
/// Stateless encoder — no dynamic table, no Huffman output.
/// Every header is encoded as either an indexed field or a literal field.
pub struct HpackEncoder;

impl HpackEncoder {
    /// Create a new stateless encoder.
    pub fn new() -> Self {
        Self
    }

    /// Encode `headers` into HPACK format, appending bytes to `out`.
    ///
    /// Each `(name, value)` is examined against the static table:
    /// - Exact match → 1-byte indexed representation.
    /// - Name match only → literal with indexed name (no indexing into dynamic table).
    /// - No match → literal with literal name (no indexing).
    pub fn encode(&self, headers: &[(&[u8], &[u8])], out: &mut Vec<u8>) {
        for &(name, value) in headers {
            match static_lookup(name, value) {
                StaticMatch::Full(idx) => encode_indexed(idx, out),
                StaticMatch::NameOnly(idx) => encode_literal_indexed_name(idx, value, out),
                StaticMatch::None => encode_literal_new_name(name, value, out),
            }
        }
    }
}

impl Default for HpackEncoder {
    fn default() -> Self {
        Self::new()
    }
}

// ── Static table lookup ───────────────────────────────────────────────────────

enum StaticMatch {
    /// Both name and value matched at this 1-based static index.
    Full(usize),
    /// Name matched; value did not. 1-based index of the first name match.
    NameOnly(usize),
    /// No match at all.
    None,
}

/// Search the static table (indices 1–61) for name+value or name-only matches.
fn static_lookup(name: &[u8], value: &[u8]) -> StaticMatch {
    let mut name_match_idx = 0usize;
    // STATIC_TABLE[0] is the unused placeholder; real entries start at 1.
    for (i, &(sn, sv)) in STATIC_TABLE.iter().enumerate().skip(1) {
        if sn.as_bytes() == name {
            if sv.as_bytes() == value {
                return StaticMatch::Full(i);
            }
            if name_match_idx == 0 {
                name_match_idx = i;
            }
        }
    }
    if name_match_idx > 0 {
        StaticMatch::NameOnly(name_match_idx)
    } else {
        StaticMatch::None
    }
}

// ── Encoding primitives ───────────────────────────────────────────────────────

/// Indexed Header Field Representation (RFC 7541 §6.1).
/// First bit = 1, remaining 7 bits = index (varint).
fn encode_indexed(index: usize, out: &mut Vec<u8>) {
    encode_integer(index as u64, 7, 0x80, out);
}

/// Literal Header Field without Indexing — Indexed Name (RFC 7541 §6.2.2).
/// First nibble = 0x00, next 4-bit prefix holds name index; value is literal string.
fn encode_literal_indexed_name(name_index: usize, value: &[u8], out: &mut Vec<u8>) {
    // 0000_xxxx: without indexing, 4-bit name index prefix
    encode_integer(name_index as u64, 4, 0x00, out);
    encode_string(value, out);
}

/// Literal Header Field without Indexing — New Name (RFC 7541 §6.2.2).
/// First byte = 0x00, then literal name string, then literal value string.
fn encode_literal_new_name(name: &[u8], value: &[u8], out: &mut Vec<u8>) {
    out.push(0x00); // no indexing, name index = 0
    encode_string(name, out);
    encode_string(value, out);
}

// ── Integer encoding (RFC 7541 §5.1) ─────────────────────────────────────────

/// Encode `value` using `prefix_bits` bits in the first byte.
/// `first_byte_flags` provides the high bits (above the prefix) of the first byte.
pub(super) fn encode_integer(value: u64, prefix_bits: u8, first_byte_flags: u8, out: &mut Vec<u8>) {
    let max_prefix = (1u64 << prefix_bits) - 1;

    if value < max_prefix {
        out.push(first_byte_flags | value as u8);
        return;
    }

    // Value exceeds prefix — encode max in prefix, then continuation bytes.
    out.push(first_byte_flags | max_prefix as u8);
    let mut remainder = value - max_prefix;
    while remainder >= 128 {
        out.push((remainder as u8 & 0x7f) | 0x80);
        remainder >>= 7;
    }
    out.push(remainder as u8);
}

// ── String encoding (RFC 7541 §5.2) ──────────────────────────────────────────

/// Encode a string as a literal (no Huffman). MSB=0, length as 7-bit varint, then raw bytes.
fn encode_string(value: &[u8], out: &mut Vec<u8>) {
    encode_integer(value.len() as u64, 7, 0x00, out); // MSB=0 → no Huffman
    out.extend_from_slice(value);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_integer_small_value() {
        // value=10, prefix=5, flags=0 → fits in one byte
        let mut out = Vec::new();
        encode_integer(10, 5, 0x00, &mut out);
        assert_eq!(out, &[10]);
    }

    #[test]
    fn encode_integer_multi_byte() {
        // RFC 7541 §C.1.1: value=1337, prefix=5
        let mut out = Vec::new();
        encode_integer(1337, 5, 0x00, &mut out);
        assert_eq!(out, &[0x1f, 0x9a, 0x0a]);
    }

    #[test]
    fn encode_indexed_get_method() {
        // :method GET → static index 2 → 0x82
        let enc = HpackEncoder::new();
        let mut out = Vec::new();
        enc.encode(&[(&b":method"[..], &b"GET"[..])], &mut out);
        assert_eq!(out, &[0x82]);
    }

    #[test]
    fn encode_indexed_status_200() {
        // :status 200 → static index 8 → 0x88
        let enc = HpackEncoder::new();
        let mut out = Vec::new();
        enc.encode(&[(&b":status"[..], &b"200"[..])], &mut out);
        assert_eq!(out, &[0x88]);
    }

    #[test]
    fn encode_literal_indexed_name_custom_value() {
        // :status exists in static table (name only for "999") → literal indexed name
        let enc = HpackEncoder::new();
        let mut out = Vec::new();
        enc.encode(&[(&b":status"[..], &b"999"[..])], &mut out);
        // First byte: 0x00 | index=8 → 0x08
        assert_eq!(out[0], 0x08);
        // Value string: length byte + "999"
        assert_eq!(&out[1..], &[0x03, b'9', b'9', b'9']);
    }

    #[test]
    fn encode_literal_new_name() {
        // Unknown header — literal new name
        let enc = HpackEncoder::new();
        let mut out = Vec::new();
        enc.encode(&[(&b"x-custom"[..], &b"val"[..])], &mut out);
        assert_eq!(out[0], 0x00); // no indexing, name index=0
        // Name: length=8 + "x-custom"
        assert_eq!(out[1], 8);
        assert_eq!(&out[2..10], b"x-custom");
        // Value: length=3 + "val"
        assert_eq!(out[10], 3);
        assert_eq!(&out[11..14], b"val");
    }

    #[test]
    fn encode_multiple_headers() {
        let enc = HpackEncoder::new();
        let mut out = Vec::new();
        enc.encode(
            &[
                (&b":method"[..], &b"GET"[..]),
                (&b":scheme"[..], &b"https"[..]),
            ],
            &mut out,
        );
        // :method GET = 0x82, :scheme https = 0x87
        assert_eq!(out[0], 0x82);
        assert_eq!(out[1], 0x87);
    }

    #[test]
    fn encode_decode_roundtrip() {
        use crate::protocol::h2::hpack::HpackDecoder;

        let enc = HpackEncoder::new();
        let mut encoded = Vec::new();
        let headers: &[(&[u8], &[u8])] = &[
            (b":method", b"GET"),
            (b":path", b"/"),
            (b":scheme", b"https"),
            (b"x-request-id", b"abc123"),
        ];
        enc.encode(headers, &mut encoded);

        let mut dec = HpackDecoder::new(4096);
        let decoded = dec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded[0], (b":method".to_vec(), b"GET".to_vec()));
        assert_eq!(decoded[1], (b":path".to_vec(), b"/".to_vec()));
        assert_eq!(decoded[2], (b":scheme".to_vec(), b"https".to_vec()));
        assert_eq!(decoded[3].0, b"x-request-id".to_vec());
        assert_eq!(decoded[3].1, b"abc123".to_vec());
    }
}
