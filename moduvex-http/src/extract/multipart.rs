//! `multipart/form-data` body extractor — RFC 2046 boundary parsing.
//!
//! # Usage
//! ```ignore
//! use moduvex_http::extract::Multipart;
//!
//! async fn upload(mut mp: Multipart) -> Response {
//!     while let Some(field) = mp.next_field() {
//!         let name = field.name().unwrap_or("").to_string();
//!         let bytes = field.bytes().to_vec();
//!         // process field...
//!     }
//!     Response::text("ok")
//! }
//! ```

use crate::body::Body;
use crate::extract::FromRequest;
use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::status::StatusCode;

// ── Rejection ─────────────────────────────────────────────────────────────────

/// Error returned when multipart parsing fails.
#[derive(Debug)]
pub struct MultipartRejection(String);

impl IntoResponse for MultipartRejection {
    fn into_response(self) -> Response {
        Response::with_body(StatusCode::BAD_REQUEST, self.0)
            .content_type("text/plain; charset=utf-8")
    }
}

// ── Field ─────────────────────────────────────────────────────────────────────

/// A single field parsed from a `multipart/form-data` body.
pub struct Field {
    /// Value of `name` in the `Content-Disposition` header.
    name: Option<String>,
    /// Value of `filename` in the `Content-Disposition` header (for file uploads).
    file_name: Option<String>,
    /// Content-Type of this field (defaults to `text/plain` if absent).
    content_type: Option<String>,
    /// Raw field body bytes.
    data: Vec<u8>,
}

impl Field {
    /// The field name from `Content-Disposition: form-data; name="..."`.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The filename from `Content-Disposition: form-data; filename="..."`.
    /// Present only for file upload fields.
    pub fn file_name(&self) -> Option<&str> {
        self.file_name.as_deref()
    }

    /// The `Content-Type` of this field, if provided.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// The raw bytes of this field's body.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Attempt to interpret the field body as a UTF-8 string.
    pub fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }
}

// ── Multipart ─────────────────────────────────────────────────────────────────

/// Parsed `multipart/form-data` body — iterate fields via [`next_field`].
///
/// All fields are parsed eagerly on construction (the full body is already
/// in memory). Iteration is sequential.
///
/// [`next_field`]: Multipart::next_field
pub struct Multipart {
    fields: std::collections::VecDeque<Field>,
}

impl Multipart {
    /// Return the next field, or `None` when all fields are consumed.
    pub fn next_field(&mut self) -> Option<Field> {
        self.fields.pop_front()
    }

    /// Number of remaining fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// True when no fields remain.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Extract the boundary string from a `Content-Type` header value.
///
/// Expects a value like `multipart/form-data; boundary=----WebKitFormBoundary`.
/// Returns `None` if the header is absent, not multipart, or missing boundary.
pub fn extract_boundary(content_type: &str) -> Option<String> {
    // Verify it is multipart/form-data.
    let lower = content_type.to_ascii_lowercase();
    if !lower.contains("multipart/form-data") {
        return None;
    }

    // Find `boundary=` parameter (case-insensitive).
    for part in content_type.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("boundary=").or_else(|| {
            part.strip_prefix("Boundary=")
                .or_else(|| part.strip_prefix("BOUNDARY="))
        }) {
            // Strip optional surrounding quotes.
            let boundary = rest.trim().trim_matches('"');
            if !boundary.is_empty() {
                return Some(boundary.to_owned());
            }
        }
    }
    None
}

/// Parse a `Content-Disposition` header value for `name` and `filename`.
///
/// Example: `form-data; name="file"; filename="upload.txt"`
fn parse_content_disposition(value: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut file_name = None;

    for part in value.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("name=") {
            name = Some(v.trim().trim_matches('"').to_owned());
        } else if let Some(v) = part.strip_prefix("filename=") {
            file_name = Some(v.trim().trim_matches('"').to_owned());
        }
    }
    (name, file_name)
}

/// Parse multipart body bytes into a list of [`Field`]s.
///
/// `boundary` must be the raw boundary string from the `Content-Type` header
/// (without the leading `--`).
///
/// # RFC 2046 structure
/// ```text
/// --<boundary>\r\n
/// Content-Disposition: ...\r\n
/// \r\n
/// <body>\r\n
/// --<boundary>\r\n
/// ...
/// --<boundary>--\r\n
/// ```
pub fn parse_multipart(body: &[u8], boundary: &str) -> Vec<Field> {
    let delim = format!("--{boundary}");
    let delim_bytes = delim.as_bytes();
    // Between-part separator: CRLF + delimiter (used to split parts).
    let inner_sep = format!("\r\n--{boundary}");
    let inner_bytes = inner_sep.as_bytes();

    let mut fields = Vec::new();

    // Locate the first boundary (may have preamble before it).
    let first_delim = match find_bytes(body, delim_bytes) {
        Some(i) => i,
        None    => return fields,
    };
    // Position right after `--boundary`.
    let mut pos = first_delim + delim_bytes.len();

    loop {
        // After the boundary delimiter there must be `\r\n` (next part) or `--` (end).
        if pos + 2 > body.len() {
            break;
        }
        match &body[pos..pos + 2] {
            b"--" => break,      // Terminal boundary: `--boundary--`
            b"\r\n" => pos += 2, // Regular part follows.
            _ => break,          // Malformed.
        }

        // Parse part headers: scan for `\r\n\r\n`.
        let rest = &body[pos..];
        let (hdr_end, sep_len) = match find_double_crlf(rest) {
            Some(v) => v,
            None    => break,
        };
        let headers_bytes = &rest[..hdr_end];
        // Absolute start-of-body for this part.
        let abs_body_start = pos + hdr_end + sep_len;

        // Parse Content-Disposition and Content-Type headers.
        let headers_str = std::str::from_utf8(headers_bytes).unwrap_or("");
        let mut disposition: Option<String> = None;
        let mut ct: Option<String> = None;
        for line in headers_str.lines() {
            let l = line.trim();
            if let Some(v) = l.strip_prefix("Content-Disposition:")
                .or_else(|| l.strip_prefix("content-disposition:"))
            {
                disposition = Some(v.trim().to_owned());
            } else if let Some(v) = l.strip_prefix("Content-Type:")
                .or_else(|| l.strip_prefix("content-type:"))
            {
                ct = Some(v.trim().to_owned());
            }
        }

        // Find the end of this part's body by locating `\r\n--boundary` from abs_body_start.
        let (part_body, next_pos) = match find_bytes(&body[abs_body_start..], inner_bytes) {
            Some(rel_end) => {
                // Part body is body[abs_body_start .. abs_body_start+rel_end].
                let part = &body[abs_body_start..abs_body_start + rel_end];
                // After the part body comes `\r\n--boundary`; next pos is right after `--boundary`.
                let next = abs_body_start + rel_end + inner_bytes.len();
                (part, next)
            }
            None => {
                // Last part — no following \r\n--boundary. Use remaining body, stripping trailing \r\n.
                let raw = &body[abs_body_start..];
                let trimmed = raw.strip_suffix(b"\r\n").unwrap_or(raw);
                (trimmed, body.len())
            }
        };

        let (name, file_name) = disposition
            .as_deref()
            .map(parse_content_disposition)
            .unwrap_or((None, None));

        fields.push(Field {
            name,
            file_name,
            content_type: ct,
            data: part_body.to_vec(),
        });

        pos = next_pos;
        if pos >= body.len() {
            break;
        }
    }

    fields
}

// ── Byte-search helpers ───────────────────────────────────────────────────────

/// Find the first occurrence of `needle` in `haystack`.
/// Returns the byte offset, or `None`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Find `\r\n\r\n` or `\n\n` in `slice`.
/// Returns `(offset_of_separator_start, separator_len)` or `None`.
fn find_double_crlf(slice: &[u8]) -> Option<(usize, usize)> {
    // Try \r\n\r\n first (standard).
    if let Some(pos) = slice.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((pos, 4));
    }
    // Fall back to \n\n (bare LF, non-standard but seen in practice).
    if let Some(pos) = slice.windows(2).position(|w| w == b"\n\n") {
        return Some((pos, 2));
    }
    None
}

// ── FromRequest impl ──────────────────────────────────────────────────────────

impl FromRequest for Multipart {
    type Rejection = MultipartRejection;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers
            .get_str("content-type")
            .ok_or_else(|| MultipartRejection("missing Content-Type header".to_string()))?
            .to_owned();

        let boundary = extract_boundary(&content_type)
            .ok_or_else(|| {
                MultipartRejection(
                    "Content-Type must be multipart/form-data with a boundary parameter"
                        .to_string(),
                )
            })?;

        let body = std::mem::replace(&mut req.body, Body::Empty);
        let bytes = body.into_bytes();

        let fields = parse_multipart(&bytes, &boundary);

        Ok(Multipart {
            fields: fields.into(),
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_multipart_body(boundary: &str, parts: &[(&str, &str, &[u8])]) -> Vec<u8> {
        // Each part: (content-disposition, optional content-type, body)
        let mut body = Vec::new();
        for (cd, ct, data) in parts {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: {cd}\r\n").as_bytes(),
            );
            if !ct.is_empty() {
                body.extend_from_slice(
                    format!("Content-Type: {ct}\r\n").as_bytes(),
                );
            }
            body.extend_from_slice(b"\r\n");
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    // ── extract_boundary ───────────────────────────────────────────────────

    #[test]
    fn extract_boundary_standard() {
        let ct = "multipart/form-data; boundary=----WebKitFormBoundary";
        assert_eq!(
            extract_boundary(ct),
            Some("----WebKitFormBoundary".to_string())
        );
    }

    #[test]
    fn extract_boundary_quoted() {
        let ct = r#"multipart/form-data; boundary="my-boundary""#;
        assert_eq!(extract_boundary(ct), Some("my-boundary".to_string()));
    }

    #[test]
    fn extract_boundary_not_multipart_returns_none() {
        assert!(extract_boundary("application/json").is_none());
    }

    #[test]
    fn extract_boundary_missing_param_returns_none() {
        assert!(extract_boundary("multipart/form-data").is_none());
    }

    // ── parse_content_disposition ─────────────────────────────────────────

    #[test]
    fn parse_disposition_name_only() {
        let (name, fname) = parse_content_disposition(r#"form-data; name="field1""#);
        assert_eq!(name.as_deref(), Some("field1"));
        assert!(fname.is_none());
    }

    #[test]
    fn parse_disposition_name_and_filename() {
        let (name, fname) = parse_content_disposition(
            r#"form-data; name="file"; filename="upload.png""#,
        );
        assert_eq!(name.as_deref(), Some("file"));
        assert_eq!(fname.as_deref(), Some("upload.png"));
    }

    // ── parse_multipart ───────────────────────────────────────────────────

    #[test]
    fn parse_multipart_two_text_fields() {
        let boundary = "boundary123";
        let body = make_multipart_body(
            boundary,
            &[
                (r#"form-data; name="username""#, "", b"alice"),
                (r#"form-data; name="age""#,      "", b"30"),
            ],
        );
        let fields = parse_multipart(&body, boundary);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name(), Some("username"));
        assert_eq!(fields[0].text(), Some("alice"));
        assert_eq!(fields[1].name(), Some("age"));
        assert_eq!(fields[1].text(), Some("30"));
    }

    #[test]
    fn parse_multipart_file_field() {
        let boundary = "testboundary";
        let body = make_multipart_body(
            boundary,
            &[(
                r#"form-data; name="file"; filename="test.txt""#,
                "text/plain",
                b"file content here",
            )],
        );
        let fields = parse_multipart(&body, boundary);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].file_name(), Some("test.txt"));
        assert_eq!(fields[0].content_type(), Some("text/plain"));
        assert_eq!(fields[0].bytes(), b"file content here");
    }

    #[test]
    fn parse_multipart_empty_body_no_fields() {
        let fields = parse_multipart(b"", "boundary");
        assert!(fields.is_empty());
    }

    #[test]
    fn parse_multipart_binary_field() {
        let boundary = "binbound";
        let binary_data = vec![0x00u8, 0xFF, 0x10, 0x20, 0x30];
        let body = make_multipart_body(
            boundary,
            &[(r#"form-data; name="data""#, "application/octet-stream", &binary_data)],
        );
        let fields = parse_multipart(&body, boundary);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].bytes(), binary_data.as_slice());
    }

    // ── Multipart extractor ───────────────────────────────────────────────

    #[test]
    fn multipart_extractor_parses_fields() {
        use crate::routing::method::Method;

        let boundary = "formbound";
        let raw = make_multipart_body(
            boundary,
            &[
                (r#"form-data; name="title""#, "", b"Hello"),
                (r#"form-data; name="body""#,  "", b"World"),
            ],
        );

        let mut req = Request::new(Method::POST, "/upload");
        req.headers.insert(
            "content-type",
            format!("multipart/form-data; boundary={boundary}").into_bytes(),
        );
        req.body = Body::Fixed(raw);

        let mut mp = Multipart::from_request(&mut req).unwrap();
        let f1 = mp.next_field().unwrap();
        assert_eq!(f1.name(), Some("title"));
        assert_eq!(f1.text(), Some("Hello"));
        let f2 = mp.next_field().unwrap();
        assert_eq!(f2.name(), Some("body"));
        assert_eq!(f2.text(), Some("World"));
        assert!(mp.next_field().is_none());
    }

    #[test]
    fn multipart_extractor_missing_content_type_fails() {
        use crate::routing::method::Method;

        let mut req = Request::new(Method::POST, "/upload");
        req.body = Body::Fixed(b"data".to_vec());

        assert!(Multipart::from_request(&mut req).is_err());
    }

    #[test]
    fn multipart_extractor_wrong_content_type_fails() {
        use crate::routing::method::Method;

        let mut req = Request::new(Method::POST, "/upload");
        req.headers.insert("content-type", b"application/json".to_vec());
        req.body = Body::Fixed(b"{}".to_vec());

        assert!(Multipart::from_request(&mut req).is_err());
    }

    #[test]
    fn multipart_len_and_is_empty() {
        use crate::routing::method::Method;

        let boundary = "b";
        let raw = make_multipart_body(
            boundary,
            &[(r#"form-data; name="x""#, "", b"1")],
        );

        let mut req = Request::new(Method::POST, "/");
        req.headers.insert(
            "content-type",
            format!("multipart/form-data; boundary={boundary}").into_bytes(),
        );
        req.body = Body::Fixed(raw);

        let mp = Multipart::from_request(&mut req).unwrap();
        assert_eq!(mp.len(), 1);
        assert!(!mp.is_empty());
    }

    // ── Field accessors ───────────────────────────────────────────────────

    #[test]
    fn field_text_invalid_utf8_returns_none() {
        let f = Field {
            name: None,
            file_name: None,
            content_type: None,
            data: vec![0xFF, 0xFE],
        };
        assert!(f.text().is_none());
    }
}
