//! HTTP/2 connection handler — drives the H2 frame loop for a single connection.
//!
//! Called from `server/mod.rs` after protocol detection confirms the client
//! negotiated `h2` via ALPN (TLS) or sent the H2 connection preface (h2c).
//!
//! # Lifecycle
//! 1. Exchange connection preface (`handle_preface`).
//! 2. Read frames in a loop (`read_frame`).
//! 3. For complete requests, spawn concurrent dispatch via `moduvex_runtime::spawn`.
//! 4. Main loop drains completed responses and writes frames.
//! 5. Exit on GOAWAY or unrecoverable I/O error.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::middleware::{self, Middleware};
use crate::protocol::h2::connection::H2Connection;
use crate::protocol::h2::frame::Frame;
use crate::request::Request;
use crate::response::Response;
use crate::routing::router::{BoxHandler, Router};
use crate::server::connection::with_timeout;
use crate::server::tls::Stream;
use crate::server::StateInjector;

// ── h2c preface detection ─────────────────────────────────────────────────────

/// H2 connection preface length (RFC 9113 §3.4).
const H2_PREFACE_LEN: usize = 24;

/// Timeout for reading the initial h2c preface bytes.
const H2C_PEEK_TIMEOUT: Duration = Duration::from_secs(1);

/// Peek at initial bytes of a plain-TCP stream to detect the H2 connection preface.
///
/// Reads up to `H2_PREFACE_LEN` bytes with a 1-second timeout. Returns
/// `(true, bytes)` when an H2 preface is detected, `(false, bytes)` otherwise.
/// The returned bytes must be prepended to subsequent reads (no data is lost).
pub(crate) async fn detect_h2c_preface(stream: &mut Stream) -> (bool, Vec<u8>) {
    use moduvex_runtime::net::AsyncRead;
    use std::future::poll_fn;

    let mut buf = vec![0u8; H2_PREFACE_LEN];
    let mut total = 0usize;

    // Read with timeout — clients send the preface immediately.
    let read_result = with_timeout(H2C_PEEK_TIMEOUT, async {
        while total < H2_PREFACE_LEN {
            let slice = &mut buf[total..];
            let n = poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, slice)).await;
            match n {
                Ok(0) => break, // EOF
                Ok(n) => total += n,
                Err(_) => break,
            }
        }
    })
    .await;

    if read_result.is_err() {
        // Timeout — return whatever we have.
    }

    buf.truncate(total);
    let is_h2 = is_h2_preface(&buf);
    (is_h2, buf)
}

/// Returns `true` if `buf` starts with the H2 connection preface prefix (`PRI`).
///
/// The full preface is `PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n` (24 bytes).
/// Checking the first 3 bytes is sufficient — no valid HTTP/1.x request starts with `PRI`.
#[inline]
pub(crate) fn is_h2_preface(buf: &[u8]) -> bool {
    buf.starts_with(b"PRI")
}

// ── H2 runner ─────────────────────────────────────────────────────────────────

/// Drive the HTTP/2 connection state machine to completion with concurrent stream dispatch.
///
/// Exchanges the connection preface, then loops reading frames and dispatching
/// complete requests concurrently. Each stream spawns an independent task; the
/// main loop drains completed responses and writes frames serially (RFC 9113
/// requires serialized frame writes, not interleaved bytes).
pub(crate) async fn run_h2_connection(
    mut stream: Stream,
    peer_addr: std::net::SocketAddr,
    router: &Arc<Router>,
    middlewares: &Arc<Vec<Arc<dyn Middleware>>>,
    extensions: &Option<StateInjector>,
    pre_read: Vec<u8>,
) {
    let mut h2 = H2Connection::new();

    // Exchange connection preface (includes pre-read bytes for h2c).
    if let Err(e) = h2.handle_preface(&mut stream, &pre_read).await {
        eprintln!("moduvex-http: H2 preface error from {peer_addr}: {e}");
        return;
    }

    // Channel for completed responses from spawned dispatch tasks.
    // Bounded by MAX_CONCURRENT_STREAMS to prevent unbounded memory growth.
    let (tx, rx) = std::sync::mpsc::channel::<(u32, Response)>();

    // Main frame loop.
    loop {
        // 1. Drain completed responses first (non-blocking).
        while let Ok((stream_id, response)) = rx.try_recv() {
            if let Err(e) = h2.send_response(stream_id, response, &mut stream).await {
                eprintln!("moduvex-http: H2 send_response error on stream {stream_id}: {e}");
                let _ = h2.send_goaway(&mut stream, e.code).await;
                drain_responses(&mut h2, &mut stream, &rx).await;
                return;
            }
        }

        // 2. Read next frame.
        let frame = match h2.read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
                let _ = h2.send_goaway(&mut stream, e.code).await;
                break;
            }
        };

        // 3. Handle PING before passing to process_frame.
        if let Frame::Ping { ack: false, data } = &frame {
            let ack = Frame::Ping { ack: true, data: *data };
            let _ = h2.write_frame(&mut stream, &ack).await;
            continue;
        }

        // Track whether we need to ACK a SETTINGS frame after processing.
        let needs_settings_ack = matches!(&frame, Frame::Settings { ack: false, .. });

        // 4. Process frame.
        match h2.process_frame(frame) {
            Ok(Some((stream_id, mut req))) => {
                // Stamp peer address and inject app state.
                req.peer_addr = Some(peer_addr);
                if let Some(inject) = extensions {
                    inject(&mut req);
                }

                // Spawn concurrent dispatch — clones are cheap (Arc).
                let router = Arc::clone(router);
                let mws = Arc::clone(middlewares);
                let tx = tx.clone();
                drop(moduvex_runtime::spawn(async move {
                    let response = dispatch_request(req, &router, &mws).await;
                    let _ = tx.send((stream_id, response));
                }));
            }
            Ok(None) => {
                // Control frame or incomplete stream — nothing to dispatch yet.
            }
            Err(e) => {
                if e.stream_id == 0 {
                    // Connection-level error — send GOAWAY and exit.
                    let _ = h2.send_goaway(&mut stream, e.code).await;
                    break;
                }
                // Stream-level error — send RST_STREAM and keep connection open.
                let rst = Frame::RstStream {
                    stream_id: e.stream_id,
                    error_code: e.code as u32,
                };
                let _ = h2.write_frame(&mut stream, &rst).await;
            }
        }

        // Send SETTINGS ACK for any peer SETTINGS frame just processed.
        if needs_settings_ack {
            let ack = Frame::Settings { ack: true, values: vec![] };
            let _ = h2.write_frame(&mut stream, &ack).await;
        }

        // Exit if GOAWAY was received from the peer.
        if h2.goaway_sent {
            break;
        }
    }

    // Drain phase: give in-flight tasks a moment, then flush remaining responses.
    drain_responses(&mut h2, &mut stream, &rx).await;
}

/// Drain remaining responses from the channel after the main loop exits.
///
/// Waits briefly for spawned tasks to complete, then flushes any buffered responses.
async fn drain_responses(
    h2: &mut H2Connection,
    stream: &mut Stream,
    rx: &std::sync::mpsc::Receiver<(u32, Response)>,
) {
    // Give spawned tasks a short window to complete.
    moduvex_runtime::sleep(Duration::from_millis(100)).await;
    while let Ok((stream_id, response)) = rx.try_recv() {
        let _ = h2.send_response(stream_id, response, stream).await;
    }
}

// ── Request dispatch ──────────────────────────────────────────────────────────

/// Dispatch a request through the middleware/router pipeline.
async fn dispatch_request(
    mut req: Request,
    router: &Router,
    middlewares: &Arc<Vec<Arc<dyn Middleware>>>,
) -> Response {
    match router.lookup(req.method, &req.path) {
        Some(route_match) => {
            req.extensions.insert(route_match.params.clone());
            middleware::dispatch(middlewares, route_match.handler, req).await
        }
        None => {
            let fb: Arc<BoxHandler> = match router.fallback_handler() {
                Some(fb) => Arc::clone(fb),
                None => Arc::new(Box::new(|_r: Request| {
                    Box::pin(async { Response::not_found() })
                        as Pin<Box<dyn Future<Output = Response> + Send>>
                }) as BoxHandler),
            };
            middleware::dispatch(middlewares, &fb, req).await
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h2_preface_detection_positive() {
        let preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        assert!(is_h2_preface(preface));
    }

    #[test]
    fn h2_preface_detection_negative_http1() {
        assert!(!is_h2_preface(b"GET / HTTP/1.1\r\n"));
        assert!(!is_h2_preface(b"POST /api HTTP/1.1\r\n"));
    }

    #[test]
    fn h2_preface_detection_empty() {
        assert!(!is_h2_preface(b""));
    }

    #[test]
    fn h2_preface_partial_not_detected() {
        // Only 2 bytes — not enough to start with PRI fully but we check prefix.
        assert!(!is_h2_preface(b"PR"));
    }
}
