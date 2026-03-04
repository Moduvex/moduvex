//! `TypeMap` — runtime type-keyed storage for singleton DI.
//!
//! Uses `HashMap<TypeId, Box<dyn Any + Send + Sync>>` under an `RwLock`.
//! This cost is **startup-only**: singletons are inserted during the Init
//! phase and then retrieved as typed `Arc<T>` — no further `TypeId` lookups
//! occur on the request hot-path.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// ── TypeMap ───────────────────────────────────────────────────────────────────

/// A thread-safe map from `TypeId` to boxed values.
///
/// All inserted values must be `Send + Sync + 'static`.
pub struct TypeMap {
    inner: RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync + 'static>>>,
}

impl TypeMap {
    /// Create an empty `TypeMap`.
    pub fn new() -> Self {
        Self { inner: RwLock::new(HashMap::new()) }
    }

    /// Insert a value of type `T`, automatically wrapping it in `Arc<T>`.
    ///
    /// Overwrites any previously stored value of the same type.
    /// Use `insert_arc` if you already have an `Arc<T>`.
    pub fn insert<T: Any + Send + Sync + 'static>(&self, value: T) {
        self.insert_arc(Arc::new(value));
    }

    /// Retrieve a reference to the stored value of type `T`, or `None`.
    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let map = self.inner.read().expect("TypeMap read lock poisoned");
        map.get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<Arc<T>>())
            .cloned()
    }

    /// Insert an `Arc<T>` — the canonical way to store singletons.
    pub fn insert_arc<T: Any + Send + Sync + 'static>(&self, value: Arc<T>) {
        let mut map = self.inner.write().expect("TypeMap write lock poisoned");
        map.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// Returns `true` if a value of type `T` is stored.
    pub fn contains<T: Any + Send + Sync + 'static>(&self) -> bool {
        let map = self.inner.read().expect("TypeMap read lock poisoned");
        map.contains_key(&TypeId::of::<T>())
    }

    /// Number of stored entries.
    pub fn len(&self) -> usize {
        let map = self.inner.read().expect("TypeMap read lock poisoned");
        map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for TypeMap {
    fn default() -> Self { Self::new() }
}

// TypeMap is Send + Sync because RwLock<HashMap<...>> is Send + Sync
// when the values are Send + Sync, which they are (bounded above).
unsafe impl Send for TypeMap {}
unsafe impl Sync for TypeMap {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get_arc() {
        let map = TypeMap::new();
        let val: Arc<u32> = Arc::new(42);
        map.insert_arc(val.clone());
        let got = map.get::<u32>().expect("should be present");
        assert_eq!(*got, 42);
    }

    #[test]
    fn missing_type_returns_none() {
        let map = TypeMap::new();
        assert!(map.get::<String>().is_none());
    }

    #[test]
    fn overwrite_replaces_value() {
        let map = TypeMap::new();
        map.insert_arc(Arc::new(1u32));
        map.insert_arc(Arc::new(2u32));
        assert_eq!(*map.get::<u32>().unwrap(), 2);
    }

    #[test]
    fn contains_reflects_presence() {
        let map = TypeMap::new();
        assert!(!map.contains::<String>());
        map.insert_arc(Arc::new("hello".to_string()));
        assert!(map.contains::<String>());
    }

    #[test]
    fn len_counts_distinct_types() {
        let map = TypeMap::new();
        map.insert_arc(Arc::new(1u32));
        map.insert_arc(Arc::new("hi".to_string()));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn thread_safe_concurrent_read() {
        use std::thread;
        let map = Arc::new(TypeMap::new());
        map.insert_arc(Arc::new(99u32));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let m = Arc::clone(&map);
                thread::spawn(move || {
                    assert_eq!(*m.get::<u32>().unwrap(), 99);
                })
            })
            .collect();
        for h in handles { h.join().unwrap(); }
    }
}
