//! `Query` extractor — parse URL query string into key-value pairs.

use std::collections::HashMap;

use crate::request::Request;
use crate::response::Response;
use crate::status::StatusCode;

use super::FromRequest;

// ── Query extractor ──────────────────────────────────────────────────────────

/// Extracted query-string parameters as a `HashMap<String, String>`.
///
/// Access individual parameters via `.get("key")`.
pub struct Query(pub HashMap<String, String>);

impl Query {
    /// Get a query parameter by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
}

impl FromRequest for Query {
    type Rejection = Response;

    fn from_request(req: &mut Request) -> Result<Self, Self::Rejection> {
        let map = match &req.query {
            Some(qs) => parse_query_string(qs),
            None => HashMap::new(),
        };
        Ok(Query(map))
    }
}

/// Parse `key=value&key2=value2` into a HashMap.
/// Performs basic percent-decoding of `+` (space) and `%XX` sequences.
fn parse_query_string(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let value = parts.next().unwrap_or("");
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

/// Minimal percent-decoding: `+` → space, `%XX` → byte.
fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => { out.push(b' '); i += 1; }
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                    16,
                ) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            ch => { out.push(ch); i += 1; }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::method::Method;

    #[test]
    fn query_extract_basic() {
        let mut req = Request::new(Method::GET, "/search");
        req.query = Some("q=rust&page=1".to_string());
        let Query(map) = Query::from_request(&mut req).unwrap();
        assert_eq!(map.get("q").map(|s| s.as_str()), Some("rust"));
        assert_eq!(map.get("page").map(|s| s.as_str()), Some("1"));
    }

    #[test]
    fn query_extract_empty() {
        let mut req = Request::new(Method::GET, "/");
        let Query(map) = Query::from_request(&mut req).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn percent_decode_plus_and_hex() {
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%25"), "100%");
    }
}
