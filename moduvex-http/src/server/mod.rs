//! HTTP server — binds TCP listener, accepts connections, dispatches requests.
//!
//! # Usage
//! ```ignore
//! use moduvex_http::prelude::*;
//!
//! async fn hello(_req: Request) -> Response { Response::text("Hello, World!") }
//!
//! fn main() {
//!     HttpServer::bind("0.0.0.0:8080")
//!         .get("/", hello)
//!         .serve();
//! }
//! ```
//!
//! # Graceful shutdown
//! ```ignore
//! use moduvex_core::lifecycle::ShutdownHandle;
//!
//! let shutdown = ShutdownHandle::new();
//! let shutdown_clone = shutdown.clone();
//!
//! // Trigger from another thread / signal handler.
//! std::thread::spawn(move || {
//!     std::thread::sleep(std::time::Duration::from_secs(5));
//!     shutdown_clone.request();
//! });
//!
//! HttpServer::bind("0.0.0.0:8080")
//!     .get("/", hello)
//!     .with_shutdown(shutdown)
//!     .serve()
//!     .unwrap();
//! ```

pub mod connection;
pub mod listener;
pub mod tls;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use moduvex_core::lifecycle::ShutdownHandle;
use moduvex_runtime::{block_on_with_spawn, spawn};

use crate::extract::IntoHandler;
use crate::middleware::Middleware;
use crate::request::Request;
use crate::routing::method::Method;
use crate::routing::router::Router;

use connection::{with_timeout, ConnConfig, Connection};
use listener::Listener;

/// Type alias for the state injector function.
type StateInjector = Arc<dyn Fn(&mut Request) + Send + Sync>;

/// Default grace period to wait for in-flight requests to complete on shutdown.
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval for the shutdown check in the accept loop (no OS-level select).
/// Keeping this low (≤100ms) ensures shutdown latency stays sub-second.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

// ── HttpServer ────────────────────────────────────────────────────────────────

/// Builder for the HTTP server. Configure routes, middleware, then `serve()`.
pub struct HttpServer {
    addr: SocketAddr,
    router: Router,
    config: ConnConfig,
    middlewares: Vec<Arc<dyn Middleware>>,
    /// Type-erased state injector — inserts T into each request's extensions.
    state_injector: Option<StateInjector>,
    /// Maximum number of concurrent connections (0 = unlimited).
    max_connections: usize,
    /// Optional handle for graceful shutdown. If `None`, a default handle is
    /// created and the server runs until the process is terminated.
    shutdown_handle: Option<ShutdownHandle>,
    /// How long to wait for in-flight connections to finish after shutdown is
    /// requested before forcibly exiting. Default: 30 seconds.
    drain_timeout: Duration,
    #[cfg(feature = "tls")]
    tls_config: Option<tls::TlsConfig>,
}

impl HttpServer {
    /// Create a new server builder bound to `addr`.
    ///
    /// # Panics
    /// Panics if `addr` is not a valid socket address string.
    pub fn bind(addr: &str) -> Self {
        let addr: SocketAddr = addr.parse().expect("invalid bind address");
        Self {
            addr,
            router: Router::new(),
            config: ConnConfig::default(),
            middlewares: Vec::new(),
            state_injector: None,
            max_connections: 0,
            shutdown_handle: None,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            #[cfg(feature = "tls")]
            tls_config: None,
        }
    }

    /// Register a route for any HTTP method.
    pub fn route<T>(mut self, method: Method, pattern: &str, handler: impl IntoHandler<T>) -> Self {
        self.router = self.router.route(method, pattern, handler);
        self
    }

    /// Register a GET route.
    pub fn get<T>(mut self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.router = self.router.get(pattern, h);
        self
    }

    /// Register a POST route.
    pub fn post<T>(mut self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.router = self.router.post(pattern, h);
        self
    }

    /// Register a PUT route.
    pub fn put<T>(mut self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.router = self.router.put(pattern, h);
        self
    }

    /// Register a DELETE route.
    pub fn delete<T>(mut self, pattern: &str, h: impl IntoHandler<T>) -> Self {
        self.router = self.router.delete(pattern, h);
        self
    }

    /// Set a custom fallback handler for unmatched routes.
    pub fn fallback<T>(mut self, handler: impl IntoHandler<T>) -> Self {
        self.router = self.router.fallback(handler);
        self
    }

    /// Add a middleware to the pipeline (executed in registration order).
    pub fn middleware(mut self, mw: impl Middleware) -> Self {
        self.middlewares.push(Arc::new(mw));
        self
    }

    /// Inject shared state accessible via `State<T>` extractor.
    pub fn state<T: Clone + Send + Sync + 'static>(mut self, val: T) -> Self {
        self.state_injector = Some(Arc::new(move |req: &mut Request| {
            req.extensions.insert(val.clone());
        }));
        self
    }

    /// Override connection configuration (limits and timeouts).
    pub fn config(mut self, config: ConnConfig) -> Self {
        self.config = config;
        self
    }

    /// Set maximum concurrent connections (default: unlimited).
    pub fn max_connections(mut self, limit: usize) -> Self {
        self.max_connections = limit;
        self
    }

    /// Attach a `ShutdownHandle` for graceful shutdown.
    ///
    /// When `handle.request()` is called, the server stops accepting new
    /// connections and waits up to `drain_timeout` for in-flight requests to
    /// complete before returning from `serve()`.
    pub fn with_shutdown(mut self, handle: ShutdownHandle) -> Self {
        self.shutdown_handle = Some(handle);
        self
    }

    /// Set the grace period for in-flight connections to drain after shutdown
    /// is requested. Default: 30 seconds. Connections still open after this
    /// window are abandoned (the tasks are dropped).
    pub fn drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = timeout;
        self
    }

    /// Set TLS configuration (requires `tls` feature).
    #[cfg(feature = "tls")]
    pub fn tls(mut self, config: tls::TlsConfig) -> Self {
        self.tls_config = Some(config);
        self
    }

    /// Start the server, blocking the current thread until shutdown completes.
    ///
    /// If no `ShutdownHandle` was provided via `with_shutdown()`, a private
    /// default handle is used and the server runs indefinitely (existing
    /// behaviour — backward compatible).
    pub fn serve(self) -> std::io::Result<()> {
        let listener = Listener::bind(self.addr)?;
        let actual_addr = listener.local_addr();
        eprintln!("moduvex-http: listening on http://{actual_addr}");

        let router = Arc::new(self.router);
        let config = self.config;
        let middlewares = Arc::new(self.middlewares);
        let state_injector = self.state_injector;

        let max_conns = self.max_connections;
        let drain_timeout = self.drain_timeout;

        // Use provided handle or create an internal one. A caller who never
        // calls `request()` on the internal handle is equivalent to the old
        // infinite-loop behaviour — the server only exits on process kill.
        let shutdown = self.shutdown_handle.unwrap_or_default();

        block_on_with_spawn(async move {
            let active_conns = Arc::new(AtomicUsize::new(0));

            // ── Accept loop ───────────────────────────────────────────────────
            // We poll `listener.accept()` with a short timeout so that the
            // shutdown flag is checked regularly without blocking indefinitely.
            loop {
                // Check shutdown before attempting to accept.
                if shutdown.is_requested() {
                    break;
                }

                // Enforce connection limit (0 = unlimited).
                if max_conns > 0 && active_conns.load(Ordering::Acquire) >= max_conns {
                    // Yield to let existing connections make progress and close.
                    moduvex_runtime::sleep(Duration::from_millis(1)).await;
                    continue;
                }

                // Race accept() against a short poll interval. On timeout we
                // loop back to the shutdown check without blocking forever.
                match with_timeout(ACCEPT_POLL_INTERVAL, listener.accept()).await {
                    Ok(Ok((stream, peer_addr))) => {
                        let router = Arc::clone(&router);
                        let config = config.clone();
                        let mws = Arc::clone(&middlewares);
                        let inj = state_injector.clone();
                        let conns = Arc::clone(&active_conns);
                        conns.fetch_add(1, Ordering::AcqRel);
                        drop(spawn(async move {
                            let conn = Connection::new(stream, peer_addr, config);
                            conn.run(&router, &mws, &inj).await;
                            conns.fetch_sub(1, Ordering::AcqRel);
                        }));
                    }
                    Ok(Err(e)) => {
                        eprintln!("moduvex-http: accept error: {e}");
                        // continue — transient errors are handled inside Listener
                    }
                    Err(()) => {
                        // Poll interval elapsed — re-check shutdown flag and loop.
                    }
                }
            }

            // ── Drain phase ───────────────────────────────────────────────────
            // Wait for active connections to finish up to `drain_timeout`.
            // Each connection's own idle/write timeouts bound individual tasks,
            // so the drain period is just a hard upper bound for the group.
            eprintln!("moduvex-http: shutting down — draining {active_conns_val} in-flight connection(s)",
                active_conns_val = active_conns.load(Ordering::Acquire));

            let drain_start = std::time::Instant::now();
            while active_conns.load(Ordering::Acquire) > 0 {
                if drain_start.elapsed() >= drain_timeout {
                    eprintln!(
                        "moduvex-http: drain timeout elapsed, abandoning {} connection(s)",
                        active_conns.load(Ordering::Acquire)
                    );
                    break;
                }
                moduvex_runtime::sleep(Duration::from_millis(50)).await;
            }

            eprintln!("moduvex-http: server stopped");
        });

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream as StdTcpStream;
    use std::time::{Duration, Instant};

    // ── Shutdown handle integration ───────────────────────────────────────

    #[test]
    fn server_stops_accepting_after_shutdown_request() {
        // Bind to an ephemeral port, request shutdown immediately, then serve()
        // should return quickly because the accept loop checks the flag on each
        // iteration (within ACCEPT_POLL_INTERVAL + drain time).
        async fn handler(_req: crate::request::Request) -> crate::response::Response {
            crate::response::Response::text("ok")
        }

        let shutdown = ShutdownHandle::new();
        let shutdown_trigger = shutdown.clone();

        // Request shutdown before serve() even enters the loop.
        shutdown_trigger.request();

        let start = Instant::now();
        HttpServer::bind("127.0.0.1:0")
            .get("/", handler)
            .with_shutdown(shutdown)
            .drain_timeout(Duration::from_millis(100))
            .serve()
            .expect("serve failed");

        // Should finish well under 1 second.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "serve took too long after shutdown: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn server_drains_and_stops_when_no_connections() {
        // With no in-flight connections the drain loop exits immediately.
        async fn handler(_req: crate::request::Request) -> crate::response::Response {
            crate::response::Response::text("ok")
        }

        let shutdown = ShutdownHandle::new();
        let trigger = shutdown.clone();
        trigger.request();

        let start = Instant::now();
        HttpServer::bind("127.0.0.1:0")
            .get("/", handler)
            .with_shutdown(shutdown)
            .drain_timeout(Duration::from_secs(5))
            .serve()
            .expect("serve failed");

        // No connections → drain completes immediately.
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn server_handles_request_before_shutdown() {
        // Start server, send one request, then shut down.
        async fn handler(_req: crate::request::Request) -> crate::response::Response {
            crate::response::Response::text("hello")
        }

        let shutdown = ShutdownHandle::new();
        let trigger = shutdown.clone();

        // Bind first to discover the port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        // Run server in background thread.
        let server_thread = std::thread::spawn(move || {
            HttpServer::bind(&format!("127.0.0.1:{port}"))
                .get("/", handler)
                .with_shutdown(shutdown)
                .drain_timeout(Duration::from_millis(200))
                .serve()
                .expect("serve failed");
        });

        // Give the server a moment to bind and start accepting.
        std::thread::sleep(Duration::from_millis(150));

        // Send a simple HTTP/1.0 request (no keep-alive, so connection closes).
        let mut stream = StdTcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200"), "expected 200 OK, got: {resp}");

        // Trigger shutdown and wait for the server thread to exit.
        trigger.request();
        server_thread.join().expect("server thread panicked");
    }

    #[test]
    fn default_drain_timeout_is_30s() {
        assert_eq!(DEFAULT_DRAIN_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn builder_with_shutdown_sets_handle() {
        // Verify builder chain compiles and attaches handle correctly.
        async fn handler(_req: crate::request::Request) -> crate::response::Response {
            crate::response::Response::text("ok")
        }
        let shutdown = ShutdownHandle::new();
        let trigger = shutdown.clone();
        trigger.request(); // pre-signal so serve exits immediately

        HttpServer::bind("127.0.0.1:0")
            .get("/", handler)
            .with_shutdown(shutdown)
            .drain_timeout(Duration::from_millis(50))
            .serve()
            .expect("serve failed");
    }
}
