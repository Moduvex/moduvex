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
        Self {
            inner: RwLock::new(HashMap::new()),
        }
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
    fn default() -> Self {
        Self::new()
    }
}

// TypeMap auto-derives Send + Sync: RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>
// is Send + Sync when inner values are Send + Sync (which they are, bounded above).

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
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn insert_value_wraps_in_arc() {
        let map = TypeMap::new();
        map.insert(42u64);
        let got = map.get::<u64>().unwrap();
        assert_eq!(*got, 42u64);
    }

    #[test]
    fn distinct_types_are_independent() {
        let map = TypeMap::new();
        map.insert_arc(Arc::new(1u32));
        map.insert_arc(Arc::new("hello".to_string()));
        map.insert_arc(Arc::new(3.14f64));
        assert_eq!(*map.get::<u32>().unwrap(), 1);
        assert_eq!(map.get::<String>().unwrap().as_str(), "hello");
        assert!((map.get::<f64>().unwrap().abs() - 3.14f64).abs() < f64::EPSILON);
    }

    #[test]
    fn is_empty_on_new_map() {
        let map = TypeMap::new();
        assert!(map.is_empty());
    }

    #[test]
    fn is_empty_false_after_insert() {
        let map = TypeMap::new();
        map.insert(99u8);
        assert!(!map.is_empty());
    }

    #[test]
    fn len_after_multiple_types() {
        let map = TypeMap::new();
        map.insert(1u8);
        map.insert(2u16);
        map.insert(3u32);
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn overwrite_does_not_increase_len() {
        let map = TypeMap::new();
        map.insert_arc(Arc::new(1u32));
        map.insert_arc(Arc::new(2u32));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn default_creates_empty_map() {
        let map = TypeMap::default();
        assert!(map.is_empty());
    }

    #[test]
    fn concurrent_write_then_read() {
        use std::thread;
        let map = Arc::new(TypeMap::new());
        // Each thread inserts its own type
        let m1 = Arc::clone(&map);
        let t1 = thread::spawn(move || m1.insert_arc(Arc::new(10u8)));
        let m2 = Arc::clone(&map);
        let t2 = thread::spawn(move || m2.insert_arc(Arc::new(20u16)));
        t1.join().unwrap();
        t2.join().unwrap();
        // Both insertions should be visible
        assert!(map.get::<u8>().is_some());
        assert!(map.get::<u16>().is_some());
    }

    #[test]
    fn arc_ptr_eq_for_same_insert() {
        let map = TypeMap::new();
        let val = Arc::new(42u32);
        map.insert_arc(Arc::clone(&val));
        let got = map.get::<u32>().unwrap();
        assert!(Arc::ptr_eq(&val, &got));
    }

    #[test]
    fn struct_types_as_keys() {
        #[derive(Debug, PartialEq)]
        struct MyService { id: u32 }

        let map = TypeMap::new();
        map.insert_arc(Arc::new(MyService { id: 42 }));
        let svc = map.get::<MyService>().unwrap();
        assert_eq!(svc.id, 42);
    }
}
