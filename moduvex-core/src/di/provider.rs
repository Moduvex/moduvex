//! DI provider abstractions — `Provider`, `Singleton`, `RequestScoped`, `Inject`.
//!
//! Singletons are stored as `Arc<T>` in `AppContext` and resolved once at
//! startup. Request-scoped values are created per-request via a factory
//! closure stored in `RequestScoped<T>`.

use std::sync::Arc;

use crate::app::context::{AppContext, RequestContext};
use crate::error::Result;

/// Type alias for a request-scoped factory closure.
type RequestFactory<T> = dyn Fn(&RequestContext) -> Result<T> + Send + Sync;

// ── Provider ──────────────────────────────────────────────────────────────────

/// A factory that produces a value of type `Output` from the `AppContext`.
///
/// Implement this for types that know how to construct themselves from
/// registered singletons. Phase 4 macros generate implementations automatically.
pub trait Provider: Send + Sync + 'static {
    /// The type this provider produces.
    type Output: Send + Sync + 'static;

    /// Produce a value, reading required dependencies from `ctx`.
    fn provide(&self, ctx: &AppContext) -> Result<Self::Output>;
}

// ── Singleton ─────────────────────────────────────────────────────────────────

/// A singleton value wrapper: created once at Init, shared via `Arc<T>`.
///
/// The canonical handle to a registered singleton inside `AppContext`.
/// Callers receive an `Arc<T>` clone — no `TypeId` lookup on the hot path.
pub struct Singleton<T: Send + Sync + 'static> {
    value: Arc<T>,
}

impl<T: Send + Sync + 'static> Singleton<T> {
    /// Wrap an already-constructed value.
    pub fn new(value: T) -> Self {
        Self {
            value: Arc::new(value),
        }
    }

    /// Wrap an existing `Arc<T>`.
    pub fn from_arc(arc: Arc<T>) -> Self {
        Self { value: arc }
    }

    /// Get a clone of the inner `Arc<T>`.
    pub fn get(&self) -> Arc<T> {
        Arc::clone(&self.value)
    }
}

impl<T: Send + Sync + 'static> Clone for Singleton<T> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
        }
    }
}

// ── RequestScoped ─────────────────────────────────────────────────────────────

/// A request-scoped value: created fresh for every `RequestContext`.
///
/// The factory closure captures `AppContext`-level dependencies at
/// registration time so per-request creation is cheap.
pub struct RequestScoped<T: Send + Sync + 'static> {
    factory: Arc<RequestFactory<T>>,
}

impl<T: Send + Sync + 'static> RequestScoped<T> {
    /// Create a `RequestScoped` provider from a factory function.
    pub fn new<F>(factory: F) -> Self
    where
        F: Fn(&RequestContext) -> Result<T> + Send + Sync + 'static,
    {
        Self {
            factory: Arc::new(factory),
        }
    }

    /// Invoke the factory to produce a fresh `T` for the given request context.
    pub fn create(&self, ctx: &RequestContext) -> Result<T> {
        (self.factory)(ctx)
    }
}

impl<T: Send + Sync + 'static> Clone for RequestScoped<T> {
    fn clone(&self) -> Self {
        Self {
            factory: Arc::clone(&self.factory),
        }
    }
}

// ── Inject ────────────────────────────────────────────────────────────────────

/// A type that can resolve itself from the `AppContext`.
///
/// The Phase 4 macro layer derives this for `#[injectable]` structs.
/// Hand-implementing is also supported for custom resolution logic.
///
/// Unlike a blanket impl (which would require specialization), each injectable
/// type implements `Inject` explicitly — either by hand or via macro.
pub trait Inject: Sized + Send + Sync + 'static {
    /// Resolve `Self` from the application context.
    ///
    /// Returns `Err(ModuvexError::Config)` if a required singleton is absent.
    fn resolve(ctx: &AppContext) -> Result<Self>;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singleton_get_clones_arc() {
        let s = Singleton::new(42u32);
        let a = s.get();
        let b = s.get();
        assert_eq!(*a, 42);
        // Same allocation — both Arcs point to the same value.
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn singleton_from_arc() {
        let arc = Arc::new("hello".to_string());
        let s = Singleton::from_arc(Arc::clone(&arc));
        assert_eq!(*s.get(), "hello");
    }

    #[test]
    fn singleton_clone_shares_arc() {
        let s = Singleton::new(7u64);
        let s2 = s.clone();
        assert!(Arc::ptr_eq(&s.get(), &s2.get()));
    }

    #[test]
    fn request_scoped_creates_fresh_value() {
        let app = Arc::new(AppContext::new());
        let rs: RequestScoped<u32> = RequestScoped::new(|_ctx| Ok(99));
        let req = RequestContext::new(Arc::clone(&app));
        assert_eq!(rs.create(&req).unwrap(), 99);
    }

    #[test]
    fn request_scoped_clone_shares_factory() {
        let app = Arc::new(AppContext::new());
        let rs: RequestScoped<String> = RequestScoped::new(|_| Ok("req-value".to_string()));
        let rs2 = rs.clone();
        let req = RequestContext::new(Arc::clone(&app));
        assert_eq!(rs2.create(&req).unwrap(), "req-value");
    }

    #[test]
    fn singleton_new_wraps_value() {
        let s = Singleton::new("test".to_string());
        assert_eq!(s.get().as_str(), "test");
    }

    #[test]
    fn singleton_from_arc_preserves_arc() {
        let arc = Arc::new(100u32);
        let s = Singleton::from_arc(Arc::clone(&arc));
        assert!(Arc::ptr_eq(&arc, &s.get()));
    }

    #[test]
    fn singleton_multiple_get_calls_return_same_arc() {
        let s = Singleton::new(55u64);
        let a = s.get();
        let b = s.get();
        let c = s.get();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&b, &c));
    }

    #[test]
    fn singleton_clone_preserves_value() {
        let s = Singleton::new(vec![1u32, 2, 3]);
        let s2 = s.clone();
        assert_eq!(*s2.get(), vec![1u32, 2, 3]);
    }

    #[test]
    fn request_scoped_factory_receives_context() {
        let app = Arc::new(AppContext::new());
        app.insert(Arc::new(42u32));

        // Factory reads from app context
        let rs: RequestScoped<u32> = RequestScoped::new(|ctx| {
            ctx.get::<u32>().map(|v| *v).ok_or_else(|| {
                crate::error::ModuvexError::Config(
                    crate::error::classify::ConfigError::new("missing u32")
                )
            })
        });

        let req = RequestContext::new(Arc::clone(&app));
        let result = rs.create(&req).unwrap();
        assert_eq!(result, 42u32);
    }

    #[test]
    fn request_scoped_returns_error_when_factory_fails() {
        let app = Arc::new(AppContext::new());
        let rs: RequestScoped<u32> = RequestScoped::new(|_| {
            Err(crate::error::ModuvexError::Config(
                crate::error::classify::ConfigError::new("factory error")
            ))
        });
        let req = RequestContext::new(Arc::clone(&app));
        assert!(rs.create(&req).is_err());
    }

    #[test]
    fn request_scoped_creates_independent_values_per_call() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc as StdArc;

        let counter = StdArc::new(AtomicU32::new(0));
        let counter_clone = StdArc::clone(&counter);

        let app = Arc::new(AppContext::new());
        let rs: RequestScoped<u32> = RequestScoped::new(move |_| {
            let val = counter_clone.fetch_add(1, Ordering::SeqCst);
            Ok(val)
        });

        let req = RequestContext::new(Arc::clone(&app));
        let v1 = rs.create(&req).unwrap();
        let v2 = rs.create(&req).unwrap();
        // Each call produces a fresh value
        assert_ne!(v1, v2);
    }
}
