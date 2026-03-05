//! HTTP/1.1 response encoder — serialises a `Response` into wire bytes.
//!
//! Writes directly into a `Vec<u8>` output buffer:
//!   `HTTP/1.1 <status>\r\n<headers>\r\n<body>`
//!
//! Automatically injects `Content-Length` for `Fixed` bodies and
//! `Transfer-Encoding: chunked` for `Stream` bodies.

use crate::body::Body;
use crate::response::Response;
use crate::status::StatusCode;

/// Encode a `Response` into `out`, ready to write to a TCP stream.
///
/// For `Body::Fixed` the `Content-Length` header is set automatically.
/// For `Body::Stream` `Transfer-Encoding: chunked` is used.
/// For `Body::Empty` no body headers are added (0-length assumed).
pub fn encode_response(response: Response, out: &mut Vec<u8>) {
    let status = response.status;
    let headers = response.headers;
    let body = response.body;

    // Determine body framing up front.
    let (content_length, use_chunked) = match &body {
        Body::Empty => (Some(0usize), false),
        Body::Fixed(v) => (Some(v.len()), false),
        Body::Stream(_) => (None, true),
    };

    // Status line.
    write_status_line(out, status);

    // User-supplied headers (skip content-length / transfer-encoding — we set them).
    for (name, value) in headers.iter() {
        let lower = name.to_ascii_lowercase();
        if lower == "content-length" || lower == "transfer-encoding" {
            continue; // we control these
        }
        write_header(out, name.as_bytes(), value);
    }

    // Inject framing header.
    if use_chunked {
        write_header(out, b"Transfer-Encoding", b"chunked");
    } else if let Some(len) = content_length {
        let len_str = len.to_string();
        write_header(out, b"Content-Length", len_str.as_bytes());
    }

    // End of headers.
    out.extend_from_slice(b"\r\n");

    // Body bytes.
    match body {
        Body::Empty => {}
        Body::Fixed(v) => out.extend_from_slice(&v),
        Body::Stream(rx) => {
            // Drain already-queued chunks as chunked TE.
            loop {
                match rx.next_chunk() {
                    Some(chunk) if !chunk.is_empty() => {
                        write_chunk(out, &chunk);
                    }
                    _ => break,
                }
            }
            // Terminating chunk.
            out.extend_from_slice(b"0\r\n\r\n");
        }
    }
}

/// Write a minimal error response (no user headers, plain-text body).
pub fn encode_error(status: StatusCode, msg: &str, out: &mut Vec<u8>) {
    write_status_line(out, status);
    let len = msg.len().to_string();
    write_header(out, b"Content-Type", b"text/plain; charset=utf-8");
    write_header(out, b"Content-Length", len.as_bytes());
    write_header(out, b"Connection", b"close");
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(msg.as_bytes());
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_status_line(out: &mut Vec<u8>, status: StatusCode) {
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(status.as_u16().to_string().as_bytes());
    let reason = status.canonical_reason();
    if !reason.is_empty() {
        out.push(b' ');
        out.extend_from_slice(reason.as_bytes());
    }
    out.extend_from_slice(b"\r\n");
}

fn write_header(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.extend_from_slice(name);
    out.extend_from_slice(b": ");
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
}

/// Write a single chunked-TE chunk: `<hex-size>\r\n<data>\r\n`.
fn write_chunk(out: &mut Vec<u8>, data: &[u8]) {
    let hex = format!("{:x}", data.len());
    out.extend_from_slice(hex.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::Body;
    use crate::response::Response;
    use crate::status::StatusCode;

    fn encode(r: Response) -> Vec<u8> {
        let mut out = Vec::new();
        encode_response(r, &mut out);
        out
    }

    #[test]
    fn empty_200() {
        let r = Response::new(StatusCode::OK);
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("Content-Length: 0\r\n"));
        assert!(s.contains("\r\n\r\n"));
    }

    #[test]
    fn fixed_body() {
        let r = Response::text("hello");
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("Content-Length: 5\r\n"));
        assert!(s.ends_with("hello"));
    }

    #[test]
    fn chunked_body() {
        let (body, sender) = Body::channel();
        sender.send(b"abc".to_vec());
        sender.close();
        let mut r = Response::new(StatusCode::OK);
        r.body = body;
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("Transfer-Encoding: chunked\r\n"));
        assert!(s.contains("3\r\nabc\r\n"));
        assert!(s.ends_with("0\r\n\r\n"));
    }

    #[test]
    fn error_response() {
        let mut out = Vec::new();
        encode_error(StatusCode::BAD_REQUEST, "bad input", &mut out);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(s.contains("Connection: close\r\n"));
        assert!(s.ends_with("bad input"));
    }

    #[test]
    fn encode_304_not_modified_status_line() {
        // 304 Not Modified should produce correct status line
        let r = Response::new(StatusCode::NOT_MODIFIED);
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 304 Not Modified\r\n"));
    }

    #[test]
    fn encode_user_content_length_header_is_overridden() {
        // User-supplied Content-Length is ignored — encoder sets the correct one
        let mut r = Response::text("hi");
        r.headers.insert("content-length", b"999".to_vec());
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        // Must contain the correct CL (2 bytes), not 999
        assert!(s.contains("Content-Length: 2\r\n"));
        assert!(!s.contains("Content-Length: 999"));
    }

    #[test]
    fn encode_user_transfer_encoding_is_overridden() {
        // User-supplied Transfer-Encoding is suppressed — encoder controls framing
        let mut r = Response::new(StatusCode::OK);
        r.headers.insert("transfer-encoding", b"identity".to_vec());
        r.body = Body::Fixed(b"data".to_vec());
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        // Encoder uses Content-Length for Fixed bodies; user TE must be suppressed
        assert!(!s.contains("transfer-encoding: identity"));
        assert!(s.contains("Content-Length: 4"));
    }

    #[test]
    fn encode_custom_headers_preserved() {
        let mut r = Response::new(StatusCode::OK);
        r.headers.insert("x-request-id", b"abc-123".to_vec());
        r.headers.insert("x-version", b"1".to_vec());
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("x-request-id: abc-123\r\n"));
        assert!(s.contains("x-version: 1\r\n"));
    }

    #[test]
    fn encode_404_not_found() {
        let r = Response::new(StatusCode::NOT_FOUND);
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[test]
    fn encode_503_service_unavailable() {
        let r = Response::new(StatusCode::SERVICE_UNAVAILABLE);
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    }

    #[test]
    fn encode_json_response() {
        let json_body = br#"{"ok":true}"#.to_vec();
        let body_len = json_body.len(); // 11 bytes
        let r = Response::json(json_body);
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        let expected_cl = format!("Content-Length: {body_len}\r\n");
        assert!(s.contains(&expected_cl));
        assert!(s.ends_with(r#"{"ok":true}"#));
    }

    #[test]
    fn encode_chunked_stream_ends_with_terminator() {
        let (body, sender) = Body::channel();
        sender.close();
        let mut r = Response::new(StatusCode::OK);
        r.body = body;
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("Transfer-Encoding: chunked\r\n"));
        assert!(s.ends_with("0\r\n\r\n"));
    }

    #[test]
    fn encode_large_fixed_body() {
        let data = vec![b'x'; 10_000];
        let mut r = Response::new(StatusCode::OK);
        r.body = Body::Fixed(data.clone());
        let bytes = encode(r);
        assert!(bytes.ends_with(&data));
        let headers_end = bytes.len() - data.len();
        let s = std::str::from_utf8(&bytes[..headers_end]).unwrap();
        assert!(s.contains("Content-Length: 10000\r\n"));
    }

    #[test]
    fn encode_reason_phrase_present() {
        let r = Response::new(StatusCode::BAD_REQUEST);
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    }

    #[test]
    fn encode_multiple_custom_headers_all_emitted() {
        let mut r = Response::new(StatusCode::OK);
        for i in 0..5u32 {
            r.headers.insert(
                format!("x-header-{i}"),
                format!("val{i}").into_bytes(),
            );
        }
        let bytes = encode(r);
        let s = std::str::from_utf8(&bytes).unwrap();
        for i in 0..5u32 {
            assert!(
                s.contains(&format!("x-header-{i}: val{i}\r\n")),
                "missing x-header-{i}"
            );
        }
    }

    #[test]
    fn encode_error_includes_content_type_and_connection_close() {
        let mut out = Vec::new();
        encode_error(StatusCode::INTERNAL_SERVER_ERROR, "oops", &mut out);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("Content-Type: text/plain"));
        assert!(s.contains("Connection: close"));
        assert!(s.ends_with("oops"));
    }
}
