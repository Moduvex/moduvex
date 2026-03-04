//! HTTP method enum — standard methods from RFC 9110.

/// HTTP request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    GET,
    POST,
    PUT,
    DELETE,
    PATCH,
    HEAD,
    OPTIONS,
    TRACE,
}

impl Method {
    /// Parse from a raw byte slice (case-sensitive per RFC 9110).
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        match b {
            b"GET"     => Some(Self::GET),
            b"POST"    => Some(Self::POST),
            b"PUT"     => Some(Self::PUT),
            b"DELETE"  => Some(Self::DELETE),
            b"PATCH"   => Some(Self::PATCH),
            b"HEAD"    => Some(Self::HEAD),
            b"OPTIONS" => Some(Self::OPTIONS),
            b"TRACE"   => Some(Self::TRACE),
            _          => None,
        }
    }

    /// Wire representation string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GET     => "GET",
            Self::POST    => "POST",
            Self::PUT     => "PUT",
            Self::DELETE  => "DELETE",
            Self::PATCH   => "PATCH",
            Self::HEAD    => "HEAD",
            Self::OPTIONS => "OPTIONS",
            Self::TRACE   => "TRACE",
        }
    }

    /// True if this method is safe (read-only, per RFC 9110 §9.2.1).
    pub fn is_safe(self) -> bool {
        matches!(self, Self::GET | Self::HEAD | Self::OPTIONS | Self::TRACE)
    }

    /// True if this method is idempotent (per RFC 9110 §9.2.2).
    pub fn is_idempotent(self) -> bool {
        matches!(self, Self::GET | Self::HEAD | Self::PUT | Self::DELETE | Self::OPTIONS | Self::TRACE)
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known() {
        assert_eq!(Method::from_bytes(b"GET"), Some(Method::GET));
        assert_eq!(Method::from_bytes(b"POST"), Some(Method::POST));
        assert_eq!(Method::from_bytes(b"DELETE"), Some(Method::DELETE));
    }

    #[test]
    fn parse_unknown() {
        assert!(Method::from_bytes(b"CONNECT").is_none());
        assert!(Method::from_bytes(b"get").is_none()); // case-sensitive
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", Method::GET), "GET");
    }
}
