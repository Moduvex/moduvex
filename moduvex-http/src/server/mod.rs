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

pub mod connection;
pub mod listener;
pub mod tls;

use std::net::SocketAddr;
use std::sync::Arc;

use moduvex_runtime::{block_on_with_spawn, spawn};

use crate::extract::IntoHandler;
use crate::middleware::Middleware;
use crate::request::Request;
use crate::routing::method::Method;
use crate::routing::router::Router;

use connection::{ConnConfig, Connection};
use listener::Listener;

/// Type alias for the state injector function.
type StateInjector = Arc<dyn Fn(&mut Request) + Send + Sync>;

// ── HttpServer ────────────────────────────────────────────────────────────────

/// Builder for the HTTP server. Configure routes, middleware, then `serve()`.
pub struct HttpServer {
    addr: SocketAddr,
    router: Router,
    config: ConnConfig,
    middlewares: Vec<Arc<dyn Middleware>>,
    /// Type-erased state injector — inserts T into each request's extensions.
    state_injector: Option<StateInjector>,
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

    /// Override connection configuration.
    pub fn config(mut self, config: ConnConfig) -> Self {
        self.config = config;
        self
    }

    /// Set TLS configuration (requires `tls` feature).
    #[cfg(feature = "tls")]
    pub fn tls(mut self, config: tls::TlsConfig) -> Self {
        self.tls_config = Some(config);
        self
    }

    /// Start the server, blocking the current thread until the process exits.
    pub fn serve(self) -> std::io::Result<()> {
        let listener = Listener::bind(self.addr)?;
        let actual_addr = listener.local_addr();
        eprintln!("moduvex-http: listening on http://{actual_addr}");

        let router = Arc::new(self.router);
        let config = self.config;
        let middlewares = Arc::new(self.middlewares);
        let state_injector = self.state_injector;

        block_on_with_spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        let router = Arc::clone(&router);
                        let config = config.clone();
                        let mws = Arc::clone(&middlewares);
                        let inj = state_injector.clone();
                        drop(spawn(async move {
                            let conn = Connection::new(stream, peer_addr, config);
                            conn.run(&router, &mws, &inj).await;
                        }));
                    }
                    Err(e) => {
                        eprintln!("moduvex-http: accept error: {e}");
                        break;
                    }
                }
            }
        });

        Ok(())
    }
}
