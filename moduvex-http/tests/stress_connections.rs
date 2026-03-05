//! Stress test: concurrent HTTP parsing and routing.
//!
//! Run with: `cargo test -p moduvex-http -- --ignored stress`

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use moduvex_http::protocol::h1::parser::{parse_request_head, ParseLimits, ParseStatus};
use moduvex_http::request::Request;
use moduvex_http::response::Response;
use moduvex_http::routing::method::Method;
use moduvex_http::routing::router::Router;
use moduvex_http::status::StatusCode;

use moduvex_runtime::executor::block_on_multi;
use moduvex_runtime::spawn;

async fn noop(_req: Request) -> Response {
    Response::new(StatusCode::OK)
}

#[test]
#[ignore = "stress test — run with --ignored"]
fn concurrent_parse_and_route_10k() {
    const N: u64 = 10_000;
    let counter = Arc::new(AtomicU64::new(0));
    let limits = Arc::new(ParseLimits::default());

    let router = Arc::new(
        Router::new()
            .get("/", noop)
            .get("/users", noop)
            .get("/users/:id", noop)
            .post("/users", noop)
            .get("/api/v1/health", noop),
    );

    let requests: Vec<&'static [u8]> = vec![
        b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n",
        b"GET /users HTTP/1.1\r\nHost: localhost\r\n\r\n",
        b"GET /users/42 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        b"POST /users HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        b"GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    ];

    block_on_multi(
        {
            let counter = counter.clone();
            async move {
                let mut handles = Vec::with_capacity(N as usize);
                for i in 0..N {
                    let c = counter.clone();
                    let lim = limits.clone();
                    let r = router.clone();
                    let req_bytes = requests[i as usize % requests.len()];
                    handles.push(spawn(async move {
                        // Parse
                        let status = parse_request_head(req_bytes, &lim);
                        assert!(
                            matches!(status, ParseStatus::Complete(_)),
                            "parse failed for request {i}"
                        );

                        // Extract method + path for lookup
                        if let ParseStatus::Complete(head) = status {
                            let m = r.lookup(head.method, head.path);
                            assert!(m.is_some(), "route miss for {}", head.path);
                        }

                        c.fetch_add(1, Ordering::Relaxed);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            }
        },
        8,
    );

    assert_eq!(counter.load(Ordering::SeqCst), N);
}

#[test]
#[ignore = "stress test — run with --ignored"]
fn concurrent_router_lookup_contention() {
    const N: u64 = 50_000;
    let hit_count = Arc::new(AtomicU64::new(0));
    let miss_count = Arc::new(AtomicU64::new(0));

    let router = Arc::new(
        Router::new()
            .get("/users/:id", noop)
            .get("/static/*path", noop),
    );

    let paths: Vec<&'static str> = vec![
        "/users/1",
        "/users/999",
        "/static/css/app.css",
        "/not-found",
        "/users/42",
        "/static/js/bundle.js",
        "/missing/page",
    ];

    block_on_multi(
        {
            let hit_count = hit_count.clone();
            let miss_count = miss_count.clone();
            async move {
                let mut handles = Vec::with_capacity(N as usize);
                for i in 0..N {
                    let r = router.clone();
                    let h = hit_count.clone();
                    let m = miss_count.clone();
                    let path = paths[i as usize % paths.len()];
                    handles.push(spawn(async move {
                        if r.lookup(Method::GET, path).is_some() {
                            h.fetch_add(1, Ordering::Relaxed);
                        } else {
                            m.fetch_add(1, Ordering::Relaxed);
                        }
                    }));
                }
                for jh in handles {
                    jh.await.unwrap();
                }
            }
        },
        8,
    );

    let total = hit_count.load(Ordering::SeqCst) + miss_count.load(Ordering::SeqCst);
    assert_eq!(total, N);
    assert!(hit_count.load(Ordering::SeqCst) > 0);
    assert!(miss_count.load(Ordering::SeqCst) > 0);
}
