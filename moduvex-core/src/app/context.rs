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

// SAFETY: TypeMap is Send + Sync (RwLock<HashMap<...>> with Send + Sync values)
unsafe impl Send for AppContext {}
unsafe impl Sync for AppContext {}

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
}
