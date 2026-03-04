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

use crate::routing::method::Method;
use crate::request::HttpVersion;

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
    pub method:     Method,
    pub path:       &'buf str,
    pub query:      Option<&'buf str>,
    pub version:    HttpVersion,
    /// `(name, value)` pairs. Names are raw (case-preserved) ASCII.
    pub headers:    Vec<(&'buf str, &'buf [u8])>,
    /// Total bytes consumed by the request line + headers (including final \r\n\r\n).
    pub head_len:   usize,
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
    let mut has_te   = false;
    let mut cl_value: Option<u64> = None;
    let mut multi_cl = false;
    let mut has_chunked_te = false;

    for line in lines {
        if line.is_empty() { break; } // trailing CRLF guard

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

        let name_lower = name.to_ascii_lowercase();

        // Track Host presence.
        if name_lower == "host" { has_host = true; }

        // Detect Transfer-Encoding.
        if name_lower == "transfer-encoding" {
            has_te = true;
            if value.windows(7).any(|w| w.eq_ignore_ascii_case(b"chunked")) {
                has_chunked_te = true;
            }
        }

        // Detect Content-Length conflicts.
        if name_lower == "content-length" {
            let s = std::str::from_utf8(value).unwrap_or("").trim();
            match s.parse::<u64>() {
                Ok(n) => {
                    if let Some(prev) = cl_value {
                        if prev != n { multi_cl = true; }
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
    SplitCrlf { data: bytes, pos: 0 }
}

struct SplitCrlf<'a> { data: &'a [u8], pos: usize }

impl<'a> Iterator for SplitCrlf<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() { return None; }
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
fn parse_request_line<'buf>(
    line: &'buf [u8],
) -> Result<(Method, &'buf str, Option<&'buf str>, HttpVersion), ParseError> {
    // Method: up to first SP.
    let sp1 = line.iter().position(|&b| b == b' ')
        .ok_or(ParseError::BadRequestLine)?;
    let method = Method::from_bytes(&line[..sp1])
        .ok_or(ParseError::UnknownMethod)?;

    // URI: between SP1 and SP2.
    let rest = &line[sp1 + 1..];
    let sp2 = rest.iter().position(|&b| b == b' ')
        .ok_or(ParseError::BadRequestLine)?;
    let uri_bytes = &rest[..sp2];
    let uri = std::str::from_utf8(uri_bytes).map_err(|_| ParseError::BadRequestLine)?;

    // Split path and query.
    let (path, query) = match uri.find('?') {
        Some(q) => (&uri[..q], Some(&uri[q + 1..])),
        None    => (uri, None),
    };

    // Version: after SP2.
    let ver_bytes = &rest[sp2 + 1..];
    let version = match ver_bytes {
        b"HTTP/1.1" => HttpVersion::Http11,
        b"HTTP/1.0" => HttpVersion::Http10,
        _           => return Err(ParseError::UnsupportedVersion),
    };

    Ok((method, path, query, version))
}

/// Parse `"Name: value"` from a header line.
fn parse_header_line(line: &[u8]) -> Result<(&str, &[u8]), ParseError> {
    let colon = line.iter().position(|&b| b == b':')
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
    let start = b.iter().position(|&c| c != b' ' && c != b'\t').unwrap_or(b.len());
    let end   = b.iter().rposition(|&c| c != b' ' && c != b'\t').map(|i| i + 1).unwrap_or(0);
    if start >= end { b"" } else { &b[start..end] }
}

/// Check for forbidden bytes: NUL, CR, LF.
fn contains_forbidden(b: &[u8]) -> bool {
    b.iter().any(|&c| c == 0 || c == b'\r' || c == b'\n')
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ParseLimits { ParseLimits::default() }

    fn complete(buf: &[u8]) -> ParsedHead<'_> {
        match parse_request_head(buf, &limits()) {
            ParseStatus::Complete(h) => h,
            ParseStatus::Partial     => panic!("got Partial"),
            ParseStatus::Error(e)    => panic!("got Error: {e:?}"),
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
        assert!(matches!(parse_request_head(buf, &limits()), ParseStatus::Partial));
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
}
