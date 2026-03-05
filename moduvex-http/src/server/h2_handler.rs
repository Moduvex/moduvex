//! HTTP/2 connection handler — drives the H2 frame loop for a single connection.
//!
//! Called from `server/mod.rs` after protocol detection confirms the client
//! negotiated `h2` via ALPN (TLS) or sent the H2 connection preface (h2c).
//!
//! # Lifecycle
//! 1. Exchange connection preface (`handle_preface`).
//! 2. Read frames in a loop (`read_frame`).
//! 3. For complete requests, dispatch through middleware / router.
//! 4. Send response back (`send_response`).
//! 5. Exit on GOAWAY or unrecoverable I/O error.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::middleware::{self, Middleware};
use crate::protocol::h2::connection::H2Connection;
use crate::protocol::h2::frame::Frame;
use crate::request::Request;
use crate::response::Response;
use crate::routing::router::{BoxHandler, Router};
use crate::server::tls::Stream;
use crate::server::StateInjector;

// ── H2 runner ─────────────────────────────────────────────────────────────────

/// Drive the HTTP/2 connection state machine to completion.
///
/// Exchanges the connection preface, then loops reading frames and dispatching
/// complete requests through the middleware/router pipeline. Exits cleanly on
/// GOAWAY or I/O error.
pub(crate) async fn run_h2_connection(
    mut stream: Stream,
    peer_addr: std::net::SocketAddr,
    router: &Router,
    middlewares: &Arc<Vec<Arc<dyn Middleware>>>,
    extensions: &Option<StateInjector>,
) {
    let mut h2 = H2Connection::new();

    // Exchange connection preface.
    if let Err(e) = h2.handle_preface(&mut stream).await {
        eprintln!("moduvex-http: H2 preface error from {peer_addr}: {e}");
        return;
    }

    // Main frame loop.
    loop {
        let frame = match h2.read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
                let _ = h2.send_goaway(&mut stream, e.code).await;
                break;
            }
        };

        // Handle PING before passing to process_frame (which ignores it).
        if let Frame::Ping { ack: false, data } = &frame {
            let ack = Frame::Ping { ack: true, data: *data };
            let _ = h2.write_frame(&mut stream, &ack).await;
            continue;
        }

        // Remember if we need to ACK a SETTINGS frame after processing.
        let needs_settings_ack = matches!(&frame, Frame::Settings { ack: false, .. });

        match h2.process_frame(frame) {
            Ok(Some((stream_id, mut req))) => {
                // Stamp peer address.
                req.peer_addr = Some(peer_addr);

                // Inject app state.
                if let Some(inject) = extensions {
                    inject(&mut req);
                }

                let response = dispatch_request(req, router, middlewares).await;

                if let Err(e) = h2.send_response(stream_id, response, &mut stream).await {
                    eprintln!("moduvex-http: H2 send_response error on stream {stream_id}: {e}");
                    let _ = h2.send_goaway(&mut stream, e.code).await;
                    break;
                }
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

        // Send SETTINGS ACK for any peer SETTINGS frame we just processed.
        if needs_settings_ack {
            let ack = Frame::Settings { ack: true, values: vec![] };
            let _ = h2.write_frame(&mut stream, &ack).await;
        }

        // Exit if GOAWAY was received from the peer.
        if h2.goaway_sent {
            break;
        }
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

// ── h2c preface detection ─────────────────────────────────────────────────────

/// Peek at the first bytes already buffered to detect the H2 connection preface.
///
/// The H2 preface starts with `PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n` (24 bytes).
/// We only check the first 3 bytes (`PRI`) as a fast discriminator, since no
/// valid HTTP/1.x request starts with those bytes.
///
/// Returns `true` if the buffer starts with the H2 preface prefix.
/// Reserved for h2c (plain-TCP HTTP/2 upgrade) support.
#[allow(dead_code)]
#[inline]
pub(crate) fn is_h2_preface(buf: &[u8]) -> bool {
    buf.starts_with(b"PRI")
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
}
