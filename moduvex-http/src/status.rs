//! HTTP status codes — all standard codes from RFC 9110.

/// HTTP status code.
///
/// Carries the numeric code; `as_u16()` for the integer, `canonical_reason()`
/// for the standard reason phrase used in status lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusCode(u16);

impl StatusCode {
    // ── 1xx Informational ────────────────────────────────────────────────────
    pub const CONTINUE: Self = Self(100);
    pub const SWITCHING_PROTOCOLS: Self = Self(101);
    pub const PROCESSING: Self = Self(102);
    pub const EARLY_HINTS: Self = Self(103);

    // ── 2xx Success ──────────────────────────────────────────────────────────
    pub const OK: Self = Self(200);
    pub const CREATED: Self = Self(201);
    pub const ACCEPTED: Self = Self(202);
    pub const NON_AUTHORITATIVE: Self = Self(203);
    pub const NO_CONTENT: Self = Self(204);
    pub const RESET_CONTENT: Self = Self(205);
    pub const PARTIAL_CONTENT: Self = Self(206);
    pub const MULTI_STATUS: Self = Self(207);
    pub const ALREADY_REPORTED: Self = Self(208);
    pub const IM_USED: Self = Self(226);

    // ── 3xx Redirection ──────────────────────────────────────────────────────
    pub const MULTIPLE_CHOICES: Self = Self(300);
    pub const MOVED_PERMANENTLY: Self = Self(301);
    pub const FOUND: Self = Self(302);
    pub const SEE_OTHER: Self = Self(303);
    pub const NOT_MODIFIED: Self = Self(304);
    pub const USE_PROXY: Self = Self(305);
    pub const TEMPORARY_REDIRECT: Self = Self(307);
    pub const PERMANENT_REDIRECT: Self = Self(308);

    // ── 4xx Client Error ─────────────────────────────────────────────────────
    pub const BAD_REQUEST: Self = Self(400);
    pub const UNAUTHORIZED: Self = Self(401);
    pub const PAYMENT_REQUIRED: Self = Self(402);
    pub const FORBIDDEN: Self = Self(403);
    pub const NOT_FOUND: Self = Self(404);
    pub const METHOD_NOT_ALLOWED: Self = Self(405);
    pub const NOT_ACCEPTABLE: Self = Self(406);
    pub const PROXY_AUTH_REQUIRED: Self = Self(407);
    pub const REQUEST_TIMEOUT: Self = Self(408);
    pub const CONFLICT: Self = Self(409);
    pub const GONE: Self = Self(410);
    pub const LENGTH_REQUIRED: Self = Self(411);
    pub const PRECONDITION_FAILED: Self = Self(412);
    pub const CONTENT_TOO_LARGE: Self = Self(413);
    pub const URI_TOO_LONG: Self = Self(414);
    pub const UNSUPPORTED_MEDIA_TYPE: Self = Self(415);
    pub const RANGE_NOT_SATISFIABLE: Self = Self(416);
    pub const EXPECTATION_FAILED: Self = Self(417);
    pub const IM_A_TEAPOT: Self = Self(418);
    pub const MISDIRECTED_REQUEST: Self = Self(421);
    pub const UNPROCESSABLE_CONTENT: Self = Self(422);
    pub const LOCKED: Self = Self(423);
    pub const FAILED_DEPENDENCY: Self = Self(424);
    pub const TOO_EARLY: Self = Self(425);
    pub const UPGRADE_REQUIRED: Self = Self(426);
    pub const PRECONDITION_REQUIRED: Self = Self(428);
    pub const TOO_MANY_REQUESTS: Self = Self(429);
    pub const REQUEST_HEADER_FIELDS_TOO_LARGE: Self = Self(431);
    pub const UNAVAILABLE_FOR_LEGAL_REASONS: Self = Self(451);

    // ── 5xx Server Error ─────────────────────────────────────────────────────
    pub const INTERNAL_SERVER_ERROR: Self = Self(500);
    pub const NOT_IMPLEMENTED: Self = Self(501);
    pub const BAD_GATEWAY: Self = Self(502);
    pub const SERVICE_UNAVAILABLE: Self = Self(503);
    pub const GATEWAY_TIMEOUT: Self = Self(504);
    pub const HTTP_VERSION_NOT_SUPPORTED: Self = Self(505);
    pub const VARIANT_ALSO_NEGOTIATES: Self = Self(506);
    pub const INSUFFICIENT_STORAGE: Self = Self(507);
    pub const LOOP_DETECTED: Self = Self(508);
    pub const NOT_EXTENDED: Self = Self(510);
    pub const NETWORK_AUTH_REQUIRED: Self = Self(511);

    /// Create from a raw `u16`. Returns `None` if not in 100–599.
    #[inline]
    pub fn from_u16(code: u16) -> Option<Self> {
        if (100..600).contains(&code) {
            Some(Self(code))
        } else {
            None
        }
    }

    /// The raw status code integer.
    #[inline]
    pub fn as_u16(self) -> u16 {
        self.0
    }

    /// True if the code is in the 2xx range.
    #[inline]
    pub fn is_success(self) -> bool {
        (200..300).contains(&self.0)
    }

    /// True if the code is in the 4xx range.
    #[inline]
    pub fn is_client_error(self) -> bool {
        (400..500).contains(&self.0)
    }

    /// True if the code is in the 5xx range.
    #[inline]
    pub fn is_server_error(self) -> bool {
        (500..600).contains(&self.0)
    }

    /// Standard IANA reason phrase for the status code, or `""` if non-standard.
    pub fn canonical_reason(self) -> &'static str {
        match self.0 {
            100 => "Continue",
            101 => "Switching Protocols",
            102 => "Processing",
            103 => "Early Hints",
            200 => "OK",
            201 => "Created",
            202 => "Accepted",
            203 => "Non-Authoritative Information",
            204 => "No Content",
            205 => "Reset Content",
            206 => "Partial Content",
            207 => "Multi-Status",
            208 => "Already Reported",
            226 => "IM Used",
            300 => "Multiple Choices",
            301 => "Moved Permanently",
            302 => "Found",
            303 => "See Other",
            304 => "Not Modified",
            305 => "Use Proxy",
            307 => "Temporary Redirect",
            308 => "Permanent Redirect",
            400 => "Bad Request",
            401 => "Unauthorized",
            402 => "Payment Required",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            406 => "Not Acceptable",
            407 => "Proxy Authentication Required",
            408 => "Request Timeout",
            409 => "Conflict",
            410 => "Gone",
            411 => "Length Required",
            412 => "Precondition Failed",
            413 => "Content Too Large",
            414 => "URI Too Long",
            415 => "Unsupported Media Type",
            416 => "Range Not Satisfiable",
            417 => "Expectation Failed",
            418 => "I'm a Teapot",
            421 => "Misdirected Request",
            422 => "Unprocessable Content",
            423 => "Locked",
            424 => "Failed Dependency",
            425 => "Too Early",
            426 => "Upgrade Required",
            428 => "Precondition Required",
            429 => "Too Many Requests",
            431 => "Request Header Fields Too Large",
            451 => "Unavailable For Legal Reasons",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            505 => "HTTP Version Not Supported",
            506 => "Variant Also Negotiates",
            507 => "Insufficient Storage",
            508 => "Loop Detected",
            510 => "Not Extended",
            511 => "Network Authentication Required",
            _ => "",
        }
    }
}

impl std::fmt::Display for StatusCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = self.canonical_reason();
        if reason.is_empty() {
            write!(f, "{}", self.0)
        } else {
            write!(f, "{} {}", self.0, reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u16_valid() {
        assert_eq!(StatusCode::from_u16(200), Some(StatusCode::OK));
        assert_eq!(StatusCode::from_u16(404), Some(StatusCode::NOT_FOUND));
        assert_eq!(
            StatusCode::from_u16(500),
            Some(StatusCode::INTERNAL_SERVER_ERROR)
        );
    }

    #[test]
    fn from_u16_invalid() {
        assert!(StatusCode::from_u16(99).is_none());
        assert!(StatusCode::from_u16(600).is_none());
    }

    #[test]
    fn category_checks() {
        assert!(StatusCode::OK.is_success());
        assert!(StatusCode::NOT_FOUND.is_client_error());
        assert!(StatusCode::INTERNAL_SERVER_ERROR.is_server_error());
        assert!(!StatusCode::OK.is_client_error());
    }

    #[test]
    fn canonical_reason_known() {
        assert_eq!(StatusCode::OK.canonical_reason(), "OK");
        assert_eq!(StatusCode::NOT_FOUND.canonical_reason(), "Not Found");
    }

    #[test]
    fn display_format() {
        assert_eq!(format!("{}", StatusCode::OK), "200 OK");
        assert_eq!(format!("{}", StatusCode::NOT_FOUND), "404 Not Found");
    }
}
