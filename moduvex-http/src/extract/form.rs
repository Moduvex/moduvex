//! `application/x-www-form-urlencoded` body extractor.
//!
//! Parses `key=value&key2=value2` bodies into a deserializable struct `T`.
//!
//! # Usage
//! ```ignore
//! use moduvex_http::extract::Form;
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct Login { username: String, password: String }
//!
//! async fn login(Form(data): Form<Login>) -> Response {
//!     Response::text(format!("hello {}", data.username))
//! }
//! ```

use serde::de::DeserializeOwned;

use crate::body::Body;
use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::status::StatusCode;

use crate::extract::FromRequest;

// ── Rejection ────────────────────────────────────────────────────────────────

/// Error returned when form parsing fails.
#[derive(Debug)]
pub struct FormRejection(String);

impl IntoResponse for FormRejection {
    fn into_response(self) -> Response {
        Response::with_body(StatusCode::UNPROCESSABLE_CONTENT, self.0)
            .content_type("text/plain; charset=utf-8")
    }
}

// ── URL-encoded parser ────────────────────────────────────────────────────────

/// Decode a percent-encoded byte `%XX` sequence.
///
/// `hi` and `lo` are the two hex nibble characters following `%`.
/// Returns `None` if either character is not a valid hex digit.
fn decode_hex_byte(hi: u8, lo: u8) -> Option<u8> {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _            => None,
        }
    }
    Some((hex_val(hi)? << 4) | hex_val(lo)?)
}

/// Percent-decode a URL-encoded byte slice.
///
/// - `+` is decoded as space (application/x-www-form-urlencoded convention).
/// - `%XX` sequences are decoded to their byte value.
/// - Invalid `%` sequences are passed through as-is.
pub fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < input.len() => {
                if let Some(byte) = decode_hex_byte(input[i + 1], input[i + 2]) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Parse `application/x-www-form-urlencoded` bytes into a list of `(key, value)` pairs.
///
/// Each pair is percent-decoded. Empty keys and values are allowed.
pub fn parse_urlencoded(body: &[u8]) -> Vec<(String, String)> {
    if body.is_empty() {
        return Vec::new();
    }
    body.split(|&b| b == b'&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, |&b| b == b'=');
            let key_raw = parts.next()?;
            let val_raw = parts.next().unwrap_or(b"");
            let key = String::from_utf8_lossy(&percent_decode(key_raw)).into_owned();
            let val = String::from_utf8_lossy(&percent_decode(val_raw)).into_owned();
            if key.is_empty() { None } else { Some((key, val)) }
        })
        .collect()
}

/// Convert `(key, value)` pairs to a JSON object string for serde deserialization.
///
/// This is a lightweight bridge — it avoids depending on a full form serde
/// crate by serializing pairs to JSON and deserializing with serde_json.
fn pairs_to_json(pairs: &[(String, String)]) -> String {
    let mut s = String::from('{');
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // Escape key and value as JSON strings.
        s.push('"');
        json_escape_into(&mut s, k);
        s.push_str("\":\"");
        json_escape_into(&mut s, v);
        s.push('"');
    }
    s.push('}');
    s
}

/// Append `text` JSON-escaped into `buf` (handles `"`, `\`, and control chars).
fn json_escape_into(buf: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '"'  => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                buf.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => buf.push(c),
        }
    }
}

// ── Form<T> extractor ─────────────────────────────────────────────────────────

/// Extractor that parses an `application/x-www-form-urlencoded` request body
/// into a deserializable struct `T`.
///
/// Consumes the request body — place after other body-free extractors.
pub struct Form<T>(pub T);

impl<T: DeserializeOwned + Send + 'static> FromRequest for Form<T> {
    type Rejection = FormRejection;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        // Verify Content-Type (lenient: we also accept missing header).
        if let Some(ct) = req.headers.get_str("content-type") {
            if !ct
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            {
                return Err(FormRejection(format!(
                    "expected content-type application/x-www-form-urlencoded, got {ct}"
                )));
            }
        }

        // Take the body (consuming it so later extractors see empty).
        let body = std::mem::replace(&mut req.body, Body::Empty);
        let bytes = body.into_bytes();

        let pairs = parse_urlencoded(&bytes);
        let json = pairs_to_json(&pairs);

        let value: T = serde_json::from_str(&json).map_err(|e| {
            FormRejection(format!("form deserialization error: {e}"))
        })?;

        Ok(Form(value))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── percent_decode ─────────────────────────────────────────────────────

    #[test]
    fn percent_decode_plain_text() {
        assert_eq!(percent_decode(b"hello"), b"hello");
    }

    #[test]
    fn percent_decode_plus_as_space() {
        assert_eq!(percent_decode(b"hello+world"), b"hello world");
    }

    #[test]
    fn percent_decode_percent_encoded() {
        assert_eq!(percent_decode(b"hello%20world"), b"hello world");
    }

    #[test]
    fn percent_decode_mixed() {
        assert_eq!(percent_decode(b"a%3Db"), b"a=b");
    }

    #[test]
    fn percent_decode_invalid_sequence_passthrough() {
        // Invalid %GG — passed through as-is.
        let result = percent_decode(b"%GG");
        assert_eq!(result[0], b'%');
    }

    #[test]
    fn percent_decode_empty() {
        assert_eq!(percent_decode(b""), b"");
    }

    // ── parse_urlencoded ───────────────────────────────────────────────────

    #[test]
    fn parse_urlencoded_simple() {
        let pairs = parse_urlencoded(b"name=Alice&age=30");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("name".to_string(), "Alice".to_string()));
        assert_eq!(pairs[1], ("age".to_string(), "30".to_string()));
    }

    #[test]
    fn parse_urlencoded_percent_encoding() {
        let pairs = parse_urlencoded(b"msg=hello%20world");
        assert_eq!(pairs[0].1, "hello world");
    }

    #[test]
    fn parse_urlencoded_empty_value() {
        let pairs = parse_urlencoded(b"key=");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("key".to_string(), "".to_string()));
    }

    #[test]
    fn parse_urlencoded_missing_value() {
        // `key` with no `=` — treated as key with empty value.
        let pairs = parse_urlencoded(b"key");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "key");
    }

    #[test]
    fn parse_urlencoded_empty_body() {
        assert!(parse_urlencoded(b"").is_empty());
    }

    #[test]
    fn parse_urlencoded_skips_empty_keys() {
        // &=value should be skipped (empty key).
        let pairs = parse_urlencoded(b"&=value&name=Bob");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "name");
    }

    // ── Form<T> extractor ──────────────────────────────────────────────────

    #[test]
    fn form_extractor_parses_struct() {
        use serde::Deserialize;
        use crate::routing::method::Method;

        #[derive(Deserialize)]
        struct Login { username: String, password: String }

        let mut req = Request::new(Method::POST, "/login");
        req.headers.insert("content-type", b"application/x-www-form-urlencoded".to_vec());
        req.body = Body::Fixed(b"username=alice&password=secret".to_vec());

        let Form(login) = Form::<Login>::from_request(&mut req).unwrap();
        assert_eq!(login.username, "alice");
        assert_eq!(login.password, "secret");
    }

    #[test]
    fn form_extractor_wrong_content_type_fails() {
        use serde::Deserialize;
        use crate::routing::method::Method;

        #[derive(Deserialize)]
        struct Dummy { x: String }

        let mut req = Request::new(Method::POST, "/");
        req.headers.insert("content-type", b"application/json".to_vec());
        req.body = Body::Fixed(b"x=1".to_vec());

        assert!(Form::<Dummy>::from_request(&mut req).is_err());
    }

    #[test]
    fn form_extractor_rejects_missing_field() {
        use serde::Deserialize;
        use crate::routing::method::Method;

        #[derive(Deserialize)]
        struct Needs { required_field: String }

        let mut req = Request::new(Method::POST, "/");
        req.body = Body::Fixed(b"other=value".to_vec());

        assert!(Form::<Needs>::from_request(&mut req).is_err());
    }

    #[test]
    fn json_escape_quotes_and_backslash() {
        let mut s = String::new();
        json_escape_into(&mut s, r#"a"b\c"#);
        assert_eq!(s, r#"a\"b\\c"#);
    }
}
