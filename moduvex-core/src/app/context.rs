//! `AppContext` and `RequestContext` — shared state containers.
//!
//! `AppContext` holds singletons registered during the Init lifecycle phase.
//! All access is immutable (`&self`) so it is safe to share across threads.
//!
//! `RequestContext` wraps per-request state and is created fresh per HTTP
//! request (or per unit-of-work boundary).

use std::sync::Arc;

use crate::di::scope::TypeMap;
use crate::error::{ModuvexError, Result};

// ── AppContext ─────────────────────────────────────────────────────────────────

/// Shared application context accessible by all modules.
///
/// Singletons are inserted once during the `Init` lifecycle phase and then
/// accessed via `Arc<T>` clones — no `TypeId` lookup on the hot path.
///
/// Thread-safety: `AppContext` is `Send + Sync` because `TypeMap` is
/// `Send + Sync` and all stored values are `Send + Sync`.
pub struct AppContext {
    singletons: TypeMap,
}

impl AppContext {
    /// Create a new, empty `AppContext`.
    pub fn new() -> Self {
        Self {
            singletons: TypeMap::new(),
        }
    }

    /// Store a singleton `Arc<T>` in the context.
    ///
    /// Overwrites any existing value of the same type `T`.
    pub fn insert<T: Send + Sync + 'static>(&self, value: Arc<T>) {
        self.singletons.insert_arc(value);
    }

    /// Retrieve a singleton `Arc<T>`, or `None` if not registered.
    ///
    /// This is the primary access method. Cloning the returned `Arc` is cheap.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.singletons.get::<T>()
    }

    /// Retrieve a singleton, returning an error if it is not registered.
    ///
    /// Use this when the dependency is expected to always be present.
    pub fn require<T: Send + Sync + 'static>(&self) -> Result<Arc<T>> {
        self.get::<T>().ok_or_else(|| {
            ModuvexError::Config(crate::error::classify::ConfigError::new(format!(
                "required singleton '{}' not found in AppContext — \
                 ensure the providing module is registered before use",
                std::any::type_name::<T>()
            )))
        })
    }

    /// Returns `true` if a singleton of type `T` is registered.
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.singletons.contains::<T>()
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new()
    }
}

// AppContext auto-derives Send + Sync: its only field (TypeMap) is Send + Sync.

// ── RequestContext ─────────────────────────────────────────────────────────────

/// Per-request context created at the request boundary.
///
/// Contains a reference to the shared `AppContext` plus any request-scoped
/// state (headers, authenticated principal, trace ID, etc.).
///
/// `RequestContext` is cheap to create — it borrows the `AppContext` via `Arc`.
pub struct RequestContext {
    /// The shared application context.
    app: Arc<AppContext>,
    /// Request-scoped key-value store (lazily populated).
    extensions: TypeMap,
}

impl RequestContext {
    /// Create a new `RequestContext` backed by the given `AppContext`.
    pub fn new(app: Arc<AppContext>) -> Self {
        Self {
            app,
            extensions: TypeMap::new(),
        }
    }

    /// Access the shared `AppContext`.
    pub fn app(&self) -> &AppContext {
        &self.app
    }

    /// Insert a request-scoped value of type `T`.
    pub fn insert<T: Send + Sync + 'static>(&self, value: T) {
        self.extensions.insert(value);
    }

    /// Retrieve a request-scoped value of type `T`, or `None`.
    pub fn get_extension<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.extensions.get::<T>()
    }

    /// Retrieve a singleton from the app context.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.app.get::<T>()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get_singleton() {
        let ctx = AppContext::new();
        ctx.insert(Arc::new(42u32));
        assert_eq!(*ctx.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn missing_returns_none() {
        let ctx = AppContext::new();
        assert!(ctx.get::<String>().is_none());
    }

    #[test]
    fn require_missing_returns_error() {
        let ctx = AppContext::new();
        let err = ctx.require::<String>().unwrap_err();
        assert!(matches!(err, ModuvexError::Config(_)));
    }

    #[test]
    fn contains_reflects_presence() {
        let ctx = AppContext::new();
        assert!(!ctx.contains::<u32>());
        ctx.insert(Arc::new(1u32));
        assert!(ctx.contains::<u32>());
    }

    #[test]
    fn request_context_delegates_to_app() {
        let app = Arc::new(AppContext::new());
        app.insert(Arc::new("hello".to_string()));
        let req = RequestContext::new(Arc::clone(&app));
        assert_eq!(*req.get::<String>().unwrap(), "hello");
    }

    #[test]
    fn request_context_local_extensions() {
        let app = Arc::new(AppContext::new());
        let req = RequestContext::new(Arc::clone(&app));
        req.insert(99u64);
        assert_eq!(*req.get_extension::<u64>().unwrap(), 99);
    }

    #[test]
    fn app_context_default_is_empty() {
        let ctx = AppContext::default();
        assert!(!ctx.contains::<u32>());
    }

    #[test]
    fn app_context_insert_overwrites_existing() {
        let ctx = AppContext::new();
        ctx.insert(Arc::new(1u32));
        ctx.insert(Arc::new(2u32));
        assert_eq!(*ctx.get::<u32>().unwrap(), 2);
    }

    #[test]
    fn app_context_require_present_returns_arc() {
        let ctx = AppContext::new();
        ctx.insert(Arc::new("hello".to_string()));
        let val = ctx.require::<String>().unwrap();
        assert_eq!(val.as_str(), "hello");
    }

    #[test]
    fn app_context_require_missing_error_message_contains_type() {
        let ctx = AppContext::new();
        let err = ctx.require::<u64>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("u64"), "error message should mention the type: {msg}");
    }

    #[test]
    fn app_context_multiple_different_types() {
        let ctx = AppContext::new();
        ctx.insert(Arc::new(42u32));
        ctx.insert(Arc::new("world".to_string()));
        ctx.insert(Arc::new(3.14f64));
        assert_eq!(*ctx.get::<u32>().unwrap(), 42);
        assert_eq!(ctx.get::<String>().unwrap().as_str(), "world");
        assert!((ctx.get::<f64>().unwrap().as_ref() - 3.14f64).abs() < f64::EPSILON);
    }

    #[test]
    fn app_context_concurrent_access() {
        use std::thread;
        let ctx = Arc::new(AppContext::new());
        ctx.insert(Arc::new(777u32));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let c = Arc::clone(&ctx);
                thread::spawn(move || {
                    assert_eq!(*c.get::<u32>().unwrap(), 777);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn request_context_get_missing_from_app_returns_none() {
        let app = Arc::new(AppContext::new());
        let req = RequestContext::new(Arc::clone(&app));
        assert!(req.get::<String>().is_none());
    }

    #[test]
    fn request_context_extension_and_app_singleton_are_independent() {
        let app = Arc::new(AppContext::new());
        app.insert(Arc::new(10u32));

        let req = RequestContext::new(Arc::clone(&app));
        req.insert(20u32); // extension-level u32

        // Extension shadows app-level for get_extension
        assert_eq!(*req.get_extension::<u32>().unwrap(), 20);
        // App-level still accessible via get
        assert_eq!(*req.get::<u32>().unwrap(), 10);
    }

    #[test]
    fn request_context_app_ref_matches_original() {
        let app = Arc::new(AppContext::new());
        app.insert(Arc::new(99u8));
        let req = RequestContext::new(Arc::clone(&app));
        // app() returns reference to the same context
        assert_eq!(*req.app().get::<u8>().unwrap(), 99);
    }

    #[test]
    fn request_context_missing_extension_returns_none() {
        let app = Arc::new(AppContext::new());
        let req = RequestContext::new(Arc::clone(&app));
        assert!(req.get_extension::<String>().is_none());
    }

    #[test]
    fn app_context_struct_singleton() {
        #[derive(Debug, PartialEq)]
        struct Config { port: u16 }

        let ctx = AppContext::new();
        ctx.insert(Arc::new(Config { port: 8080 }));
        let cfg = ctx.get::<Config>().unwrap();
        assert_eq!(cfg.port, 8080);
    }
}
