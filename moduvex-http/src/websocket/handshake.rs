//! WebSocket HTTP upgrade handshake — RFC 6455 §4.
//!
//! Validates the upgrade request and computes the `Sec-WebSocket-Accept` response header.
//!
//! # Protocol summary
//! 1. Client sends GET with `Upgrade: websocket`, `Connection: Upgrade`,
//!    `Sec-WebSocket-Key: <base64(16 random bytes)>`, `Sec-WebSocket-Version: 13`.
//! 2. Server responds `101 Switching Protocols` with
//!    `Sec-WebSocket-Accept: <base64(SHA-1(key + GUID))>`.
//! 3. Both sides switch to the WebSocket frame protocol.

use crate::request::Request;

/// RFC 6455 §1.3 — the GUID appended to the client key before SHA-1 hashing.
const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// ── SHA-1 (minimal RFC-compliant implementation) ───────────────────────────────
//
// We implement SHA-1 in-process rather than adding an external crate.
// SHA-1 is broken for general cryptographic purposes but is mandated
// by the WebSocket RFC for the handshake key derivation only.

/// Compute SHA-1(data) and return the 20-byte digest.
pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
    // Initial hash values (RFC 3174 §6.1).
    let mut h: [u32; 5] = [
        0x67452301,
        0xEFCDAB89,
        0x98BADCFE,
        0x10325476,
        0xC3D2E1F0,
    ];

    // Pre-processing: pad message to a multiple of 512 bits (64 bytes).
    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80); // append '1' bit
    while padded.len() % 64 != 56 {
        padded.push(0x00);
    }
    // Append original bit length as 64-bit big-endian.
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) block.
    for block in padded.chunks(64) {
        // Expand block into 80 words (w[0..79]).
        let mut w = [0u32; 80];
        for (i, chunk) in block.chunks(4).enumerate().take(16) {
            w[i] = u32::from_be_bytes(chunk.try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = [h[0], h[1], h[2], h[3], h[4]];

        for (i, wi) in w.iter().copied().enumerate() {
            let (f, k) = match i {
                0..=19  => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _       => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a.rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    // Produce 20-byte digest from five 32-bit words.
    let mut digest = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        digest[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

// ── Base64 encoder (RFC 4648, no padding variations) ─────────────────────────

/// Base64-encode `data` using the standard alphabet with `=` padding.
pub(crate) fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3F]);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F]);
        out.push(if chunk.len() > 1 { ALPHABET[(triple >> 6) as usize & 0x3F] } else { b'=' });
        out.push(if chunk.len() > 2 { ALPHABET[triple as usize & 0x3F] } else { b'=' });
    }
    String::from_utf8(out).expect("base64 output is always ASCII")
}

// ── Handshake validation ──────────────────────────────────────────────────────

/// Result of a successful WebSocket upgrade validation.
pub struct HandshakeAccept {
    /// The computed `Sec-WebSocket-Accept` response header value.
    pub accept_key: String,
}

/// Validate a WebSocket upgrade request and compute the accept key.
///
/// Returns `Ok(HandshakeAccept)` if the request is a valid upgrade,
/// or `Err(reason)` describing which requirement was not met.
pub fn validate_upgrade(req: &Request) -> Result<HandshakeAccept, &'static str> {
    // Must be GET.
    if req.method != crate::routing::method::Method::GET {
        return Err("WebSocket upgrade requires GET method");
    }

    // `Upgrade: websocket` (case-insensitive per RFC 7230).
    let upgrade = req
        .headers
        .get_str("upgrade")
        .unwrap_or("");
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err("missing or invalid Upgrade: websocket header");
    }

    // `Connection: Upgrade` (may be combined: `Connection: keep-alive, Upgrade`).
    let connection = req
        .headers
        .get_str("connection")
        .unwrap_or("");
    if !connection
        .split(',')
        .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
    {
        return Err("missing Connection: Upgrade header");
    }

    // `Sec-WebSocket-Version: 13` (only version we support).
    let version = req
        .headers
        .get_str("sec-websocket-version")
        .unwrap_or("");
    if version.trim() != "13" {
        return Err("Sec-WebSocket-Version must be 13");
    }

    // `Sec-WebSocket-Key` must be present.
    let client_key = req
        .headers
        .get_str("sec-websocket-key")
        .ok_or("missing Sec-WebSocket-Key header")?;

    let accept_key = compute_accept_key(client_key.trim());
    Ok(HandshakeAccept { accept_key })
}

/// Compute `Sec-WebSocket-Accept` from the client's `Sec-WebSocket-Key`.
///
/// Formula per RFC 6455 §4.2.2:
/// `accept = base64(SHA-1(client_key + WS_GUID))`
pub fn compute_accept_key(client_key: &str) -> String {
    let mut data = client_key.as_bytes().to_vec();
    data.extend_from_slice(WS_GUID);
    let digest = sha1(&data);
    base64_encode(&digest)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6455 §1.3 provides the canonical test vector.
    // client_key = "dGhlIHNhbXBsZSBub25jZQ=="
    // expected accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    const RFC_CLIENT_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
    const RFC_ACCEPT_KEY: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    #[test]
    fn sha1_rfc6455_test_vector() {
        let mut data = RFC_CLIENT_KEY.as_bytes().to_vec();
        data.extend_from_slice(WS_GUID);
        let digest = sha1(&data);
        // Expected SHA-1 bytes for the RFC test vector (decoded from the accept key).
        // We verify indirectly via the full accept key computation.
        let encoded = base64_encode(&digest);
        assert_eq!(encoded, RFC_ACCEPT_KEY);
    }

    #[test]
    fn compute_accept_key_matches_rfc_vector() {
        let accept = compute_accept_key(RFC_CLIENT_KEY);
        assert_eq!(accept, RFC_ACCEPT_KEY);
    }

    #[test]
    fn base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_encode_standard_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(base64_encode(b"f"),      "Zg==");
        assert_eq!(base64_encode(b"fo"),     "Zm8=");
        assert_eq!(base64_encode(b"foo"),    "Zm9v");
        assert_eq!(base64_encode(b"foob"),   "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"),  "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn sha1_empty_input() {
        // SHA-1("") = da39a3ee5e6b4b0d3255bfef95601890afd80709
        let digest = sha1(b"");
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn sha1_abc() {
        // SHA-1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        let digest = sha1(b"abc");
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn validate_upgrade_valid_request() {
        use crate::request::Request;
        use crate::routing::method::Method;

        let mut req = Request::new(Method::GET, "/ws");
        req.headers.insert("upgrade", b"websocket".to_vec());
        req.headers.insert("connection", b"Upgrade".to_vec());
        req.headers.insert("sec-websocket-version", b"13".to_vec());
        req.headers.insert("sec-websocket-key", RFC_CLIENT_KEY.as_bytes().to_vec());

        let result = validate_upgrade(&req);
        assert!(result.is_ok(), "expected Ok but got: {:?}", result.err());
        assert_eq!(result.unwrap().accept_key, RFC_ACCEPT_KEY);
    }

    #[test]
    fn validate_upgrade_missing_key_fails() {
        use crate::request::Request;
        use crate::routing::method::Method;

        let mut req = Request::new(Method::GET, "/ws");
        req.headers.insert("upgrade", b"websocket".to_vec());
        req.headers.insert("connection", b"Upgrade".to_vec());
        req.headers.insert("sec-websocket-version", b"13".to_vec());

        assert!(validate_upgrade(&req).is_err());
    }

    #[test]
    fn validate_upgrade_wrong_method_fails() {
        use crate::request::Request;
        use crate::routing::method::Method;

        let mut req = Request::new(Method::POST, "/ws");
        req.headers.insert("upgrade", b"websocket".to_vec());
        req.headers.insert("connection", b"Upgrade".to_vec());
        req.headers.insert("sec-websocket-version", b"13".to_vec());
        req.headers.insert("sec-websocket-key", RFC_CLIENT_KEY.as_bytes().to_vec());

        assert!(validate_upgrade(&req).is_err());
    }

    #[test]
    fn validate_upgrade_wrong_version_fails() {
        use crate::request::Request;
        use crate::routing::method::Method;

        let mut req = Request::new(Method::GET, "/ws");
        req.headers.insert("upgrade", b"websocket".to_vec());
        req.headers.insert("connection", b"Upgrade".to_vec());
        req.headers.insert("sec-websocket-version", b"8".to_vec()); // old version
        req.headers.insert("sec-websocket-key", RFC_CLIENT_KEY.as_bytes().to_vec());

        assert!(validate_upgrade(&req).is_err());
    }

    #[test]
    fn validate_upgrade_case_insensitive_upgrade_header() {
        use crate::request::Request;
        use crate::routing::method::Method;

        let mut req = Request::new(Method::GET, "/ws");
        req.headers.insert("upgrade", b"WebSocket".to_vec()); // mixed case
        req.headers.insert("connection", b"upgrade".to_vec()); // lower case
        req.headers.insert("sec-websocket-version", b"13".to_vec());
        req.headers.insert("sec-websocket-key", RFC_CLIENT_KEY.as_bytes().to_vec());

        assert!(validate_upgrade(&req).is_ok());
    }
}
