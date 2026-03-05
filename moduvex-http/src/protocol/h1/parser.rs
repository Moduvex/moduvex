//! Zero-copy HTTP/1.1 request parser.
//!
//! Parses request line and headers from a byte buffer without allocating for
//! header names/values — all slices borrow from the read buffer. Body reading
//! is handled separately by the connection layer.
//!
//! Security limits (configurable via `ParseLimits`):
//! - Max request-line: 8 KB
//! - Max single header: 8 KB
//! - Max total header block: 64 KB
//! - Max header count: 100

use crate::request::HttpVersion;
use crate::routing::method::Method;

// ── Limits ────────────────────────────────────────────────────────────────────

/// Configurable security limits for the parser.
#[derive(Debug, Clone, Copy)]
pub struct ParseLimits {
    pub max_request_line: usize,
    pub max_header_value: usize,
    pub max_header_block: usize,
    pub max_header_count: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_request_line: 8 * 1024,
            max_header_value: 8 * 1024,
            max_header_block: 64 * 1024,
            max_header_count: 100,
        }
    }
}

// ── Parse error ───────────────────────────────────────────────────────────────

/// Error returned when parsing fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Request line too long.
    RequestLineTooLong,
    /// Header block too large.
    HeadersTooLarge,
    /// Too many headers.
    TooManyHeaders,
    /// Single header value too long.
    HeaderValueTooLong,
    /// Malformed request line (missing method/path/version).
    BadRequestLine,
    /// Unknown HTTP method.
    UnknownMethod,
    /// Unsupported HTTP version.
    UnsupportedVersion,
    /// Malformed header field.
    BadHeader,
    /// Forbidden bytes (\r \n \0) in header name or value.
    HeaderInjection,
    /// Missing required `Host` header.
    MissingHost,
    /// Request contains both `Content-Length` and `Transfer-Encoding`.
    AmbiguousBody,
    /// Multiple `Content-Length` headers with differing values.
    MultipleContentLength,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

// ── Parsed request head ───────────────────────────────────────────────────────

/// Fully-parsed request head (line + headers), ready for body reading.
///
/// Lifetime `'buf` ties borrowed slices back to the original read buffer.
#[derive(Debug)]
pub struct ParsedHead<'buf> {
    pub method: Method,
    pub path: &'buf str,
    pub query: Option<&'buf str>,
    pub version: HttpVersion,
    /// `(name, value)` pairs. Names are raw (case-preserved) ASCII.
    pub headers: Vec<(&'buf str, &'buf [u8])>,
    /// Total bytes consumed by the request line + headers (including final \r\n\r\n).
    pub head_len: usize,
    pub has_chunked_te: bool,
    pub content_length: Option<u64>,
}

// ── Parser result ─────────────────────────────────────────────────────────────

/// Incremental parse result.
pub enum ParseStatus<'buf> {
    /// Headers complete — returns parsed head.
    Complete(ParsedHead<'buf>),
    /// More bytes needed.
    Partial,
    /// Parse error — connection should be closed with 400.
    Error(ParseError),
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Attempt to parse an HTTP/1.1 request head from `buf`.
///
/// `buf` must contain at least the headers; call again with more data on
/// `Partial`. On `Complete`, `head.head_len` bytes have been consumed.
pub fn parse_request_head<'buf>(buf: &'buf [u8], limits: &ParseLimits) -> ParseStatus<'buf> {
    // Find end of header block (\r\n\r\n).
    let Some(header_end) = find_header_end(buf) else {
        // Check if we already exceeded the max header block size.
        if buf.len() > limits.max_header_block {
            return ParseStatus::Error(ParseError::HeadersTooLarge);
        }
        return ParseStatus::Partial;
    };

    let head_bytes = &buf[..header_end];

    // Split into lines.
    let mut lines = split_crlf_lines(head_bytes);

    // ── Request line ──────────────────────────────────────────────────────
    let Some(request_line) = lines.next() else {
        return ParseStatus::Error(ParseError::BadRequestLine);
    };
    if request_line.len() > limits.max_request_line {
        return ParseStatus::Error(ParseError::RequestLineTooLong);
    }

    let (method, path, query, version) = match parse_request_line(request_line) {
        Ok(t) => t,
        Err(e) => return ParseStatus::Error(e),
    };

    // ── Headers ───────────────────────────────────────────────────────────
    let mut headers: Vec<(&'buf str, &'buf [u8])> = Vec::with_capacity(16);
    let mut has_host = false;
    let mut has_te = false;
    let mut cl_value: Option<u64> = None;
    let mut multi_cl = false;
    let mut has_chunked_te = false;

    for line in lines {
        if line.is_empty() {
            break;
        } // trailing CRLF guard

        if headers.len() >= limits.max_header_count {
            return ParseStatus::Error(ParseError::TooManyHeaders);
        }
        if line.len() > limits.max_header_value {
            return ParseStatus::Error(ParseError::HeaderValueTooLong);
        }

        let (name, value) = match parse_header_line(line) {
            Ok(p) => p,
            Err(e) => return ParseStatus::Error(e),
        };

        // Security: reject control bytes in name or value.
        if contains_forbidden(name.as_bytes()) || contains_forbidden(value) {
            return ParseStatus::Error(ParseError::HeaderInjection);
        }

        // Case-insensitive checks without allocating a lowercase copy.
        // Track Host presence.
        if name.eq_ignore_ascii_case("host") {
            has_host = true;
        }

        // Detect Transfer-Encoding.
        if name.eq_ignore_ascii_case("transfer-encoding") {
            has_te = true;
            if value.windows(7).any(|w| w.eq_ignore_ascii_case(b"chunked")) {
                has_chunked_te = true;
            }
        }

        // Detect Content-Length conflicts.
        if name.eq_ignore_ascii_case("content-length") {
            let s = std::str::from_utf8(value).unwrap_or("").trim();
            match s.parse::<u64>() {
                Ok(n) => {
                    if let Some(prev) = cl_value {
                        if prev != n {
                            multi_cl = true;
                        }
                    } else {
                        cl_value = Some(n);
                    }
                }
                Err(_) => return ParseStatus::Error(ParseError::BadHeader),
            }
        }

        headers.push((name, value));
    }

    // Validation.
    if version == HttpVersion::Http11 && !has_host {
        return ParseStatus::Error(ParseError::MissingHost);
    }
    if has_te && cl_value.is_some() {
        return ParseStatus::Error(ParseError::AmbiguousBody);
    }
    if multi_cl {
        return ParseStatus::Error(ParseError::MultipleContentLength);
    }

    ParseStatus::Complete(ParsedHead {
        method,
        path,
        query,
        version,
        headers,
        head_len: header_end,
        has_chunked_te,
        content_length: cl_value,
    })
}

// ── Internals ─────────────────────────────────────────────────────────────────

/// Find the byte offset just after `\r\n\r\n` in `buf`.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Split `bytes` on `\r\n` and yield each line as `&[u8]`.
fn split_crlf_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    SplitCrlf {
        data: bytes,
        pos: 0,
    }
}

struct SplitCrlf<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for SplitCrlf<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.pos..];
        match rest.windows(2).position(|w| w == b"\r\n") {
            Some(end) => {
                let line = &rest[..end];
                self.pos += end + 2;
                Some(line)
            }
            None => {
                let line = rest;
                self.pos = self.data.len();
                Some(line)
            }
        }
    }
}

/// Parse `"METHOD /path?query HTTP/1.x"`.
fn parse_request_line(
    line: &[u8],
) -> Result<(Method, &str, Option<&str>, HttpVersion), ParseError> {
    // Method: up to first SP.
    let sp1 = line
        .iter()
        .position(|&b| b == b' ')
        .ok_or(ParseError::BadRequestLine)?;
    let method = Method::from_bytes(&line[..sp1]).ok_or(ParseError::UnknownMethod)?;

    // URI: between SP1 and SP2.
    let rest = &line[sp1 + 1..];
    let sp2 = rest
        .iter()
        .position(|&b| b == b' ')
        .ok_or(ParseError::BadRequestLine)?;
    let uri_bytes = &rest[..sp2];
    let uri = std::str::from_utf8(uri_bytes).map_err(|_| ParseError::BadRequestLine)?;

    // Split path and query.
    let (path, query) = match uri.find('?') {
        Some(q) => (&uri[..q], Some(&uri[q + 1..])),
        None => (uri, None),
    };

    // Version: after SP2.
    let ver_bytes = &rest[sp2 + 1..];
    let version = match ver_bytes {
        b"HTTP/1.1" => HttpVersion::Http11,
        b"HTTP/1.0" => HttpVersion::Http10,
        _ => return Err(ParseError::UnsupportedVersion),
    };

    Ok((method, path, query, version))
}

/// Parse `"Name: value"` from a header line.
fn parse_header_line(line: &[u8]) -> Result<(&str, &[u8]), ParseError> {
    let colon = line
        .iter()
        .position(|&b| b == b':')
        .ok_or(ParseError::BadHeader)?;
    let name_bytes = &line[..colon];
    // OWS after colon.
    let value_start = colon + 1;
    let value = trim_ows(&line[value_start..]);

    let name = std::str::from_utf8(name_bytes).map_err(|_| ParseError::BadHeader)?;
    // Header names must be ASCII token characters (no spaces or control chars).
    if name.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err(ParseError::BadHeader);
    }
    Ok((name, value))
}

/// Strip leading and trailing ASCII whitespace (OWS = optional whitespace).
fn trim_ows(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|&c| c != b' ' && c != b'\t')
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|&c| c != b' ' && c != b'\t')
        .map(|i| i + 1)
        .unwrap_or(0);
    if start >= end {
        b""
    } else {
        &b[start..end]
    }
}

/// Check for forbidden bytes: NUL, CR, LF.
fn contains_forbidden(b: &[u8]) -> bool {
    b.iter().any(|&c| c == 0 || c == b'\r' || c == b'\n')
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ParseLimits {
        ParseLimits::default()
    }

    fn complete(buf: &[u8]) -> ParsedHead<'_> {
        match parse_request_head(buf, &limits()) {
            ParseStatus::Complete(h) => h,
            ParseStatus::Partial => panic!("got Partial"),
            ParseStatus::Error(e) => panic!("got Error: {e:?}"),
        }
    }

    #[test]
    fn simple_get() {
        let buf = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.method, Method::GET);
        assert_eq!(head.path, "/");
        assert_eq!(head.version, HttpVersion::Http11);
        assert_eq!(head.head_len, buf.len());
    }

    #[test]
    fn path_with_query() {
        let buf = b"GET /search?q=rust HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.path, "/search");
        assert_eq!(head.query, Some("q=rust"));
    }

    #[test]
    fn partial_returns_partial() {
        let buf = b"GET / HTTP/1.1\r\nHost: x\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Partial
        ));
    }

    #[test]
    fn missing_host_http11_rejected() {
        let buf = b"GET / HTTP/1.1\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::MissingHost)
        ));
    }

    #[test]
    fn cl_and_te_rejected() {
        let buf = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::AmbiguousBody)
        ));
    }

    #[test]
    fn header_injection_rejected() {
        let buf = b"GET / HTTP/1.1\r\nHost: x\r\nX-Foo: bar\rbaz\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::HeaderInjection)
        ));
    }

    #[test]
    fn unknown_method_rejected() {
        let buf = b"CONNECT / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::UnknownMethod)
        ));
    }

    // ── Method coverage ───────────────────────────────────────────────────

    #[test]
    fn parse_post_with_content_length() {
        let buf = b"POST /data HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.method, Method::POST);
        assert_eq!(head.content_length, Some(5));
        assert!(!head.has_chunked_te);
    }

    #[test]
    fn parse_put_method() {
        let buf = b"PUT /res/1 HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.method, Method::PUT);
    }

    #[test]
    fn parse_delete_method() {
        let buf = b"DELETE /res/1 HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.method, Method::DELETE);
    }

    #[test]
    fn parse_patch_method() {
        let buf = b"PATCH /res/1 HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.method, Method::PATCH);
    }

    #[test]
    fn parse_options_method() {
        let buf = b"OPTIONS /res HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.method, Method::OPTIONS);
    }

    #[test]
    fn parse_head_method() {
        let buf = b"HEAD / HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.method, Method::HEAD);
    }

    // ── HTTP version ──────────────────────────────────────────────────────

    #[test]
    fn parse_http10_no_host_required() {
        let buf = b"GET / HTTP/1.0\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.version, HttpVersion::Http10);
    }

    #[test]
    fn parse_unsupported_version_http09_rejected() {
        let buf = b"GET / HTTP/0.9\r\nHost: x\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::UnsupportedVersion)
        ));
    }

    #[test]
    fn parse_unsupported_version_http20_rejected() {
        let buf = b"GET / HTTP/2.0\r\nHost: x\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::UnsupportedVersion)
        ));
    }

    // ── Query string ──────────────────────────────────────────────────────

    #[test]
    fn parse_path_no_query() {
        let buf = b"GET /users HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.path, "/users");
        assert_eq!(head.query, None);
    }

    #[test]
    fn parse_empty_query_string() {
        let buf = b"GET /search? HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.path, "/search");
        assert_eq!(head.query, Some(""));
    }

    #[test]
    fn parse_query_multiple_params() {
        let buf = b"GET /s?a=1&b=2&c=3 HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.query, Some("a=1&b=2&c=3"));
    }

    // ── Header validation ─────────────────────────────────────────────────

    #[test]
    fn parse_multiple_headers_preserved() {
        let buf = b"GET / HTTP/1.1\r\nHost: x\r\nX-Foo: bar\r\nX-Bar: baz\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.headers.len(), 3); // Host + X-Foo + X-Bar
    }

    #[test]
    fn parse_header_value_leading_trailing_ows_stripped() {
        let buf = b"GET / HTTP/1.1\r\nHost:  example.com  \r\n\r\n";
        let head = complete(buf);
        let host = head
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("host"))
            .unwrap();
        assert_eq!(host.1, b"example.com");
    }

    #[test]
    fn parse_null_byte_in_header_value_rejected() {
        let buf = b"GET / HTTP/1.1\r\nHost: x\r\nX-Bad: foo\x00bar\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::HeaderInjection)
        ));
    }

    #[test]
    fn parse_lf_in_header_name_rejected() {
        // CRLF parsing will break this — result is either BadHeader or error
        let buf = b"GET / HTTP/1.1\r\nHost: x\r\nX-Bad\nName: val\r\n\r\n";
        let status = parse_request_head(buf, &limits());
        assert!(matches!(status, ParseStatus::Error(_)));
    }

    #[test]
    fn parse_multiple_cl_same_value_allowed() {
        // RFC 7230: identical Content-Length values are allowed
        let buf = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n";
        let status = parse_request_head(buf, &limits());
        assert!(matches!(status, ParseStatus::Complete(_)));
    }

    #[test]
    fn parse_multiple_cl_different_values_rejected() {
        let buf =
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nContent-Length: 10\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::MultipleContentLength)
        ));
    }

    #[test]
    fn parse_transfer_encoding_non_chunked_ok() {
        let buf = b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: gzip\r\n\r\n";
        let head = complete(buf);
        assert!(!head.has_chunked_te);
    }

    #[test]
    fn parse_chunked_te_detected() {
        let buf = b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n";
        let head = complete(buf);
        assert!(head.has_chunked_te);
    }

    // ── Limit enforcement ─────────────────────────────────────────────────

    #[test]
    fn parse_request_line_too_long_rejected() {
        let long_path = "a".repeat(9000);
        let buf = format!("GET /{long_path} HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(matches!(
            parse_request_head(buf.as_bytes(), &limits()),
            ParseStatus::Error(ParseError::RequestLineTooLong)
        ));
    }

    #[test]
    fn parse_too_many_headers_rejected() {
        let mut buf = b"GET / HTTP/1.1\r\nHost: x\r\n".to_vec();
        for i in 0..101 {
            buf.extend_from_slice(format!("X-H{i}: val\r\n").as_bytes());
        }
        buf.extend_from_slice(b"\r\n");
        assert!(matches!(
            parse_request_head(&buf, &limits()),
            ParseStatus::Error(ParseError::TooManyHeaders)
        ));
    }

    #[test]
    fn parse_header_value_too_long_rejected() {
        let long_val = "v".repeat(9000);
        let buf = format!("GET / HTTP/1.1\r\nHost: x\r\nX-Long: {long_val}\r\n\r\n");
        assert!(matches!(
            parse_request_head(buf.as_bytes(), &limits()),
            ParseStatus::Error(ParseError::HeaderValueTooLong)
        ));
    }

    #[test]
    fn parse_header_block_too_large_rejected() {
        let mut buf = b"GET / HTTP/1.1\r\nHost: x\r\n".to_vec();
        // Add headers until block exceeds 64KB without terminator
        let mut total = buf.len();
        while total < 65 * 1024 {
            let h = b"X-Pad: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n";
            buf.extend_from_slice(h);
            total += h.len();
        }
        // No \r\n\r\n terminator — triggers size check on Partial
        let status = parse_request_head(&buf, &limits());
        assert!(matches!(
            status,
            ParseStatus::Error(ParseError::HeadersTooLarge)
        ));
    }

    #[test]
    fn parse_bad_request_line_no_space_rejected() {
        let buf = b"GETHTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &limits()),
            ParseStatus::Error(ParseError::BadRequestLine)
        ));
    }

    // ── head_len correctness ──────────────────────────────────────────────

    #[test]
    fn parse_head_len_equals_consumed_bytes() {
        let buf = b"GET /path HTTP/1.1\r\nHost: example.com\r\nX-Custom: value\r\n\r\nBODY";
        let head = complete(buf);
        // head_len points exactly to start of body
        assert_eq!(&buf[head.head_len..], b"BODY");
    }

    #[test]
    fn parse_host_with_port() {
        let buf = b"GET / HTTP/1.1\r\nHost: example.com:8080\r\n\r\n";
        let head = complete(buf);
        let host = head
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("host"))
            .unwrap();
        assert_eq!(host.1, b"example.com:8080");
    }

    #[test]
    fn parse_limits_custom_tight_max_request_line() {
        let tight = ParseLimits {
            max_request_line: 10,
            ..ParseLimits::default()
        };
        let buf = b"GET /very/long/path HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(
            parse_request_head(buf, &tight),
            ParseStatus::Error(ParseError::RequestLineTooLong)
        ));
    }

    #[test]
    fn parse_http10_with_content_length() {
        let buf = b"POST /data HTTP/1.0\r\nContent-Length: 3\r\n\r\n";
        let head = complete(buf);
        assert_eq!(head.content_length, Some(3));
    }
}
