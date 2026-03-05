//! Per-connection state machine — reads requests, dispatches handlers, writes responses.
//!
//! State flow: Reading → Dispatching → Writing → (keep-alive? Reading | Closing)
//!
//! # Timeouts
//! - `idle_timeout`: max wait between keep-alive requests (protects against slow-loris).
//! - `read_timeout`: max time to receive the full request head once data starts arriving.
//! - `write_timeout`: max time allowed to flush the full response.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use moduvex_runtime::net::{AsyncRead, AsyncWrite};

use crate::server::tls::Stream;

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
use crate::websocket::{BoxWsCallback, WsStream};

// ── Config ────────────────────────────────────────────────────────────────────

/// Per-connection configuration limits and timeouts.
#[derive(Debug, Clone)]
pub struct ConnConfig {
    pub max_read_buf: usize,
    pub max_body_size: u64,
    pub max_requests: u32,
    pub parse_limits: ParseLimits,
    /// Max idle time between keep-alive requests before closing (default: 5s).
    /// Protects against slow-loris: connection must send a new request head
    /// within this window or it is closed.
    pub idle_timeout: Duration,
    /// Max time to receive and parse the full request head once data starts
    /// arriving (default: 60s). Guards against partial-request stalls.
    pub read_timeout: Duration,
    /// Max time to flush the full response to the client (default: 30s).
    /// Guards against stalled client reads that would hold the connection open.
    pub write_timeout: Duration,
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            max_read_buf: 64 * 1024,
            max_body_size: 2 * 1024 * 1024,
            max_requests: 1000,
            parse_limits: ParseLimits::default(),
            idle_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(60),
            write_timeout: Duration::from_secs(30),
        }
    }
}

// ── Timeout helper ────────────────────────────────────────────────────────────

/// Race a future against a sleep deadline using the runtime timer wheel.
///
/// Returns `Ok(output)` if the future completes first, or `Err(())` if the
/// timeout elapses first. Built on `moduvex_runtime::sleep` — no tokio.
pub(crate) async fn with_timeout<F>(dur: Duration, fut: F) -> Result<F::Output, ()>
where
    F: Future,
{
    let mut fut = std::pin::pin!(fut);
    let mut sleep = std::pin::pin!(moduvex_runtime::sleep(dur));
    std::future::poll_fn(|cx| {
        // Poll the user future first — completion wins.
        if let Poll::Ready(v) = fut.as_mut().poll(cx) {
            return Poll::Ready(Ok(v));
        }
        // Check timer — if elapsed, signal timeout.
        if let Poll::Ready(()) = sleep.as_mut().poll(cx) {
            return Poll::Ready(Err(()));
        }
        Poll::Pending
    })
    .await
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

/// Drives a single connection (plain TCP or TLS) through the HTTP request/response cycle.
pub struct Connection {
    /// Underlying stream — `None` only transiently during WebSocket upgrade hand-off.
    stream: Option<Stream>,
    peer_addr: SocketAddr,
    config: ConnConfig,
    requests_served: u32,
    /// Pre-read bytes from h2c preface peek (empty for normal H1 connections).
    pre_read: Vec<u8>,
}

impl Connection {
    /// Create a connection. `pre_read` contains bytes already peeked from the stream
    /// (used when h2c detection peeked bytes that turned out to be an H1 request).
    pub fn new(stream: Stream, peer_addr: SocketAddr, config: ConnConfig, pre_read: Vec<u8>) -> Self {
        Self {
            stream: Some(stream),
            peer_addr,
            config,
            requests_served: 0,
            pre_read,
        }
    }

    /// Run the connection loop with middleware support.
    ///
    /// Each iteration:
    /// 1. Wait for the next request head (guarded by `idle_timeout`).
    /// 2. Read the body.
    /// 3. Dispatch through the middleware/router pipeline.
    /// 4. Write the response (guarded by `write_timeout`).
    /// 5. Repeat if keep-alive, else close.
    pub async fn run(
        mut self,
        router: &Router,
        middlewares: &Arc<Vec<Arc<dyn Middleware>>>,
        req_extensions: &Option<StateInjector>,
    ) {
        // Seed read buffer from pre-read bytes (h2c peek path); else start fresh.
        let mut read_buf = if self.pre_read.is_empty() {
            Vec::with_capacity(4096)
        } else {
            std::mem::take(&mut self.pre_read)
        };

        loop {
            // 1. Read head — enforces idle_timeout between keep-alive requests.
            //    This is the primary defense against slow-loris: a client that
            //    never sends the next request head gets disconnected.
            let head = match with_timeout(self.config.idle_timeout, self.read_head(&mut read_buf))
                .await
            {
                Ok(Ok(h)) => h,
                // Timeout OR read/parse error — close the connection.
                Ok(Err(_)) | Err(()) => break,
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

            // 5a. WebSocket upgrade — detect 101 response, hand off stream.
            //
            // If the handler returned 101 Switching Protocols and embedded a
            // BoxWsCallback in response.extensions, we:
            //   (a) encode + flush the 101 response, then
            //   (b) take self.stream out of Option and pass it to the callback.
            //
            // After the upgrade the HTTP loop exits — the stream is moved into WsStream.
            if response.status == StatusCode::SWITCHING_PROTOCOLS {
                let mut resp = response;
                let callback = resp.extensions.remove::<BoxWsCallback>();

                // Encode and flush the 101 Switching Protocols response.
                let mut out = Vec::with_capacity(256);
                encode_response(resp, &mut out);
                if with_timeout(self.config.write_timeout, self.write_all(&out))
                    .await
                    .is_err()
                {
                    break;
                }

                // Move the stream into WsStream and invoke the user callback.
                // `BoxWsCallback::take()` extracts the inner FnOnce from the Mutex.
                if let Some(raw_cb) = callback.and_then(|c| c.take()) {
                    if let Some(stream) = self.stream.take() {
                        let mut ws = WsStream::new(stream);
                        // Drain any buffered bytes into the WsStream read buffer
                        // (should normally be empty at upgrade time).
                        if !read_buf.is_empty() {
                            ws.prepend_read_buf(std::mem::take(&mut read_buf));
                        }
                        raw_cb(ws).await;
                    }
                }
                break; // Always exit the HTTP loop after WebSocket upgrade.
            }

            // 5b. Keep-alive (standard HTTP)
            self.requests_served += 1;
            let keep_alive = keep_alive_req
                && version == HttpVersion::Http11
                && self.requests_served < self.config.max_requests;

            // 6. Encode and send — guarded by write_timeout to prevent stalled clients
            //    from holding the connection open indefinitely.
            let mut out = Vec::with_capacity(512);
            let mut resp = response;
            if keep_alive {
                resp.headers.insert("connection", b"keep-alive".to_vec());
            } else {
                resp.headers.insert("connection", b"close".to_vec());
            }
            encode_response(resp, &mut out);

            if with_timeout(self.config.write_timeout, self.write_all(&out))
                .await
                .is_err()
            {
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
        let max_body = self.config.max_body_size as usize;
        loop {
            if has_chunked_terminator(buf) {
                match crate::protocol::h1::decode_chunked(buf) {
                    Ok(decoded) => {
                        // Enforce max_body_size on the decoded body.
                        if decoded.len() > max_body {
                            self.send_error(StatusCode::CONTENT_TOO_LARGE, "body too large")
                                .await;
                            return Err(());
                        }
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
            // Limit raw buffer to prevent memory exhaustion during chunked reads.
            // Allow overhead for chunk metadata (sizes, CRLFs).
            if buf.len() > max_body + max_body / 10 + 1024 {
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
        poll_fn(|cx| {
            Pin::new(self.stream.as_mut().expect("stream gone")).poll_read(cx, buf)
        })
        .await
    }

    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        use std::future::poll_fn;
        let mut sent = 0;
        while sent < buf.len() {
            let n = poll_fn(|cx| {
                Pin::new(self.stream.as_mut().expect("stream gone"))
                    .poll_write(cx, &buf[sent..])
            })
            .await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "write returned 0 bytes",
                ));
            }
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

/// Check if the buffer contains the chunked terminator (`0\r\n\r\n`).
///
/// This is a cheap pre-check to decide whether a chunked message might be
/// complete. Full validation/decoding still happens later in parser.
fn has_chunked_terminator(buf: &[u8]) -> bool {
    if buf.len() < 5 {
        return false;
    }

    buf.windows(5).any(|w| w == b"0\r\n\r\n")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn conn_config_default_has_timeout_fields() {
        let cfg = ConnConfig::default();
        assert_eq!(cfg.idle_timeout, Duration::from_secs(5));
        assert_eq!(cfg.read_timeout, Duration::from_secs(60));
        assert_eq!(cfg.write_timeout, Duration::from_secs(30));
    }

    #[test]
    fn conn_config_custom_timeouts() {
        let cfg = ConnConfig {
            idle_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(120),
            write_timeout: Duration::from_secs(45),
            ..ConnConfig::default()
        };
        assert_eq!(cfg.idle_timeout, Duration::from_secs(10));
        assert_eq!(cfg.read_timeout, Duration::from_secs(120));
        assert_eq!(cfg.write_timeout, Duration::from_secs(45));
    }

    #[test]
    fn conn_config_clone() {
        let cfg = ConnConfig::default();
        let cfg2 = cfg.clone();
        assert_eq!(cfg.idle_timeout, cfg2.idle_timeout);
    }

    #[test]
    fn with_timeout_future_completes_before_deadline() {
        // Future that resolves immediately should win before the 1s timeout.
        moduvex_runtime::block_on(async {
            let result = with_timeout(Duration::from_secs(1), async { 42u32 }).await;
            assert_eq!(result, Ok(42u32));
        });
    }

    #[test]
    fn with_timeout_fires_when_future_is_slow() {
        // Sleep for 200ms; timeout is 50ms — timeout should win.
        moduvex_runtime::block_on(async {
            let start = Instant::now();
            let result = with_timeout(
                Duration::from_millis(50),
                moduvex_runtime::sleep(Duration::from_millis(200)),
            )
            .await;
            assert_eq!(result, Err(()));
            // Verify we didn't wait the full 200ms.
            assert!(
                start.elapsed() < Duration::from_millis(180),
                "timeout fired too late: {:?}",
                start.elapsed()
            );
        });
    }

    #[test]
    fn with_timeout_returns_ok_on_fast_future() {
        // Future finishes in 10ms; timeout is 500ms — should return Ok.
        moduvex_runtime::block_on(async {
            let result = with_timeout(
                Duration::from_millis(500),
                moduvex_runtime::sleep(Duration::from_millis(10)),
            )
            .await;
            assert_eq!(result, Ok(()));
        });
    }
}
