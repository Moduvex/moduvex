//! Per-connection state machine — reads requests, dispatches handlers, writes responses.
//!
//! State flow: Reading → Dispatching → Writing → (keep-alive? Reading | Closing)

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use moduvex_runtime::net::{AsyncRead, AsyncWrite, TcpStream};

use crate::body::Body;
use crate::header::HeaderMap;
use crate::middleware::{self, Middleware};
use crate::protocol::h1::{
    encode_error, encode_response, parse_request_head, ParseError, ParseLimits, ParseStatus,
};
use crate::request::{HttpVersion, Request};
use crate::response::Response;
use crate::routing::method::Method;
use crate::routing::router::{BoxHandler, Router};
use crate::server::StateInjector;
use crate::status::StatusCode;

// ── Config ────────────────────────────────────────────────────────────────────

/// Per-connection configuration limits.
#[derive(Debug, Clone)]
pub struct ConnConfig {
    pub max_read_buf: usize,
    pub max_body_size: u64,
    pub max_requests: u32,
    pub parse_limits: ParseLimits,
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            max_read_buf: 64 * 1024,
            max_body_size: 2 * 1024 * 1024,
            max_requests: 1000,
            parse_limits: ParseLimits::default(),
        }
    }
}

// ── Parsed head (owned) ───────────────────────────────────────────────────────

struct OwnedHead {
    method: Method,
    path: String,
    query: Option<String>,
    version: HttpVersion,
    headers: Vec<(String, Vec<u8>)>,
    head_len: usize,
    has_chunked_te: bool,
    content_length: Option<u64>,
}

// ── Connection ────────────────────────────────────────────────────────────────

/// Drives a single TCP connection through the HTTP request/response cycle.
pub struct Connection {
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: ConnConfig,
    requests_served: u32,
}

impl Connection {
    pub fn new(stream: TcpStream, peer_addr: SocketAddr, config: ConnConfig) -> Self {
        Self {
            stream,
            peer_addr,
            config,
            requests_served: 0,
        }
    }

    /// Run the connection loop with middleware support.
    pub async fn run(
        mut self,
        router: &Router,
        middlewares: &Arc<Vec<Arc<dyn Middleware>>>,
        req_extensions: &Option<StateInjector>,
    ) {
        let mut read_buf: Vec<u8> = Vec::with_capacity(4096);

        loop {
            // 1. Read head
            let head = match self.read_head(&mut read_buf).await {
                Ok(h) => h,
                Err(_) => break,
            };

            let head_len = head.head_len;
            let method = head.method;
            let path = head.path.clone();
            let query = head.query.clone();
            let version = head.version;
            let keep_alive_req = is_keep_alive_request(version, &head.headers);
            let cl = head.content_length;
            let is_chunked = head.has_chunked_te;

            let mut req_headers = HeaderMap::new();
            for (name, value) in &head.headers {
                req_headers.append(name.clone(), value.clone());
            }
            read_buf.drain(..head_len);

            // 2. Read body
            let body = if is_chunked {
                match self.read_chunked_body(&mut read_buf).await {
                    Ok(b) => b,
                    Err(_) => break,
                }
            } else if let Some(len) = cl {
                if len > self.config.max_body_size {
                    self.send_error(StatusCode::CONTENT_TOO_LARGE, "body too large")
                        .await;
                    break;
                }
                match self.read_fixed_body(&mut read_buf, len as usize).await {
                    Ok(b) => b,
                    Err(_) => break,
                }
            } else {
                Body::Empty
            };

            // 3. Build Request
            let mut req = Request::new(method, path);
            req.query = query;
            req.version = version;
            req.headers = req_headers;
            req.body = body;
            req.peer_addr = Some(self.peer_addr);

            // Inject per-request extensions (e.g. app state)
            if let Some(inject) = req_extensions {
                inject(&mut req);
            }

            // 4. Dispatch with middleware
            let response = match router.lookup(method, &req.path) {
                Some(route_match) => {
                    req.extensions.insert(route_match.params.clone());
                    middleware::dispatch(middlewares, route_match.handler, req).await
                }
                None => {
                    let fb = match router.fallback_handler() {
                        Some(fb) => Arc::clone(fb),
                        None => Arc::new(Box::new(|_r| {
                            Box::pin(async { Response::not_found() })
                                as std::pin::Pin<
                                    Box<dyn std::future::Future<Output = Response> + Send>,
                                >
                        }) as BoxHandler),
                    };
                    middleware::dispatch(middlewares, &fb, req).await
                }
            };

            // HEAD: strip body but preserve headers
            let response = if method == Method::HEAD {
                let mut r = Response::new(response.status);
                r.headers = response.headers;
                r
            } else {
                response
            };

            // 5. Keep-alive
            self.requests_served += 1;
            let keep_alive = keep_alive_req
                && version == HttpVersion::Http11
                && self.requests_served < self.config.max_requests;

            // 6. Encode and send
            let mut out = Vec::with_capacity(512);
            let mut resp = response;
            if keep_alive {
                resp.headers.insert("connection", b"keep-alive".to_vec());
            } else {
                resp.headers.insert("connection", b"close".to_vec());
            }
            encode_response(resp, &mut out);

            if self.write_all(&out).await.is_err() {
                break;
            }
            if !keep_alive {
                break;
            }
        }
    }

    // ── Read helpers ──────────────────────────────────────────────────────

    async fn read_head(&mut self, buf: &mut Vec<u8>) -> Result<OwnedHead, ()> {
        loop {
            let complete = {
                let slice: &[u8] = buf.as_slice();
                match parse_request_head(slice, &self.config.parse_limits) {
                    ParseStatus::Complete(head) => {
                        let owned = OwnedHead {
                            method: head.method,
                            path: head.path.to_string(),
                            query: head.query.map(str::to_string),
                            version: head.version,
                            headers: head
                                .headers
                                .iter()
                                .map(|(n, v)| (n.to_string(), v.to_vec()))
                                .collect(),
                            head_len: head.head_len,
                            has_chunked_te: head.has_chunked_te,
                            content_length: head.content_length,
                        };
                        Some(Ok(owned))
                    }
                    ParseStatus::Partial => None,
                    ParseStatus::Error(e) => Some(Err(e)),
                }
            };

            match complete {
                Some(Ok(owned)) => return Ok(owned),
                Some(Err(e)) => {
                    let status = parse_error_to_status(&e);
                    self.send_error(status, "parse error").await;
                    return Err(());
                }
                None => {
                    if buf.len() >= self.config.max_read_buf {
                        self.send_error(
                            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                            "headers too large",
                        )
                        .await;
                        return Err(());
                    }
                    let mut tmp = [0u8; 4096];
                    let n = self.read_some(&mut tmp).await.map_err(|_| ())?;
                    if n == 0 {
                        if !buf.is_empty() {
                            self.send_error(StatusCode::BAD_REQUEST, "unexpected EOF")
                                .await;
                        }
                        return Err(());
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
            }
        }
    }

    async fn read_fixed_body(&mut self, buf: &mut Vec<u8>, len: usize) -> Result<Body, ()> {
        while buf.len() < len {
            let mut tmp = [0u8; 4096];
            let n = self.read_some(&mut tmp).await.map_err(|_| ())?;
            if n == 0 {
                self.send_error(StatusCode::BAD_REQUEST, "unexpected EOF in body")
                    .await;
                return Err(());
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let body_bytes: Vec<u8> = buf.drain(..len).collect();
        Ok(Body::Fixed(body_bytes))
    }

    async fn read_chunked_body(&mut self, buf: &mut Vec<u8>) -> Result<Body, ()> {
        loop {
            if has_chunked_terminator(buf) {
                match crate::protocol::h1::decode_chunked(buf) {
                    Ok(decoded) => {
                        buf.clear();
                        return Ok(Body::Fixed(decoded));
                    }
                    Err(_) => {
                        self.send_error(StatusCode::BAD_REQUEST, "bad chunked encoding")
                            .await;
                        return Err(());
                    }
                }
            }
            if buf.len() > self.config.max_body_size as usize + 1024 {
                self.send_error(StatusCode::CONTENT_TOO_LARGE, "body too large")
                    .await;
                return Err(());
            }
            let mut tmp = [0u8; 4096];
            let n = self.read_some(&mut tmp).await.map_err(|_| ())?;
            if n == 0 {
                self.send_error(StatusCode::BAD_REQUEST, "unexpected EOF in chunked body")
                    .await;
                return Err(());
            }
            buf.extend_from_slice(&tmp[..n]);
        }
    }

    // ── Low-level I/O ─────────────────────────────────────────────────────

    async fn read_some(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::future::poll_fn;
        poll_fn(|cx| Pin::new(&mut self.stream).poll_read(cx, buf)).await
    }

    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        use std::future::poll_fn;
        let mut sent = 0;
        while sent < buf.len() {
            let n = poll_fn(|cx| Pin::new(&mut self.stream).poll_write(cx, &buf[sent..])).await?;
            sent += n;
        }
        Ok(())
    }

    async fn send_error(&mut self, status: StatusCode, msg: &str) {
        let mut out = Vec::new();
        encode_error(status, msg, &mut out);
        let _ = self.write_all(&out).await;
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn is_keep_alive_request(version: HttpVersion, headers: &[(String, Vec<u8>)]) -> bool {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("connection") {
            let v = std::str::from_utf8(value)
                .unwrap_or("")
                .to_ascii_lowercase();
            if v.contains("close") {
                return false;
            }
            if v.contains("keep-alive") {
                return true;
            }
        }
    }
    version == HttpVersion::Http11
}

fn parse_error_to_status(e: &ParseError) -> StatusCode {
    match e {
        ParseError::RequestLineTooLong
        | ParseError::HeadersTooLarge
        | ParseError::TooManyHeaders
        | ParseError::HeaderValueTooLong => StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
        ParseError::UnsupportedVersion => StatusCode::HTTP_VERSION_NOT_SUPPORTED,
        ParseError::AmbiguousBody | ParseError::MultipleContentLength => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn has_chunked_terminator(buf: &[u8]) -> bool {
    buf.windows(5).any(|w| w == b"0\r\n\r\n")
}
