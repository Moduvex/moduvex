//! Case-insensitive HTTP header map with multi-value support.
//!
//! Stores headers in a `Vec` of `(name, value)` pairs — O(n) lookup is fine
//! for typical header counts (<30). Names are stored lowercased for fast
//! case-insensitive comparison. Values are raw `Vec<u8>` bytes.

/// A single header entry.
#[derive(Debug, Clone)]
pub struct HeaderEntry {
    /// Lowercased header name (e.g. `"content-type"`).
    pub name: String,
    /// Raw header value bytes.
    pub value: Vec<u8>,
}

/// HTTP header map — case-insensitive name lookup, multi-value append.
///
/// Insertion order is preserved. Duplicate names are allowed (e.g. `Set-Cookie`).
#[derive(Debug, Clone, Default)]
pub struct HeaderMap {
    entries: Vec<HeaderEntry>,
}

impl HeaderMap {
    /// Create an empty header map.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Insert a header, replacing any existing entry with the same name.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) {
        let name = name.into().to_ascii_lowercase();
        let value = value.into();
        // Replace first occurrence; remove extras.
        let mut replaced = false;
        self.entries.retain_mut(|e| {
            if e.name == name {
                if !replaced {
                    e.value = value.clone();
                    replaced = true;
                    true
                } else {
                    false // remove duplicate
                }
            } else {
                true
            }
        });
        if !replaced {
            self.entries.push(HeaderEntry { name, value });
        }
    }

    /// Append a header, allowing multiple values with the same name.
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) {
        let name = name.into().to_ascii_lowercase();
        self.entries.push(HeaderEntry {
            name,
            value: value.into(),
        });
    }

    /// Get the first value for `name` (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let lower = name.to_ascii_lowercase();
        self.entries
            .iter()
            .find(|e| e.name == lower)
            .map(|e| e.value.as_slice())
    }

    /// Get the first value as a UTF-8 string slice.
    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|v| std::str::from_utf8(v).ok())
    }

    /// Get all values for `name` (case-insensitive).
    pub fn get_all(&self, name: &str) -> impl Iterator<Item = &[u8]> {
        let lower = name.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(move |e| e.name == lower)
            .map(|e| e.value.as_slice())
    }

    /// Remove all entries with `name`.
    pub fn remove(&mut self, name: &str) {
        let lower = name.to_ascii_lowercase();
        self.entries.retain(|e| e.name != lower);
    }

    /// True if the map contains at least one entry for `name`.
    pub fn contains(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.entries.iter().any(|e| e.name == lower)
    }

    /// Iterate over all entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries
            .iter()
            .map(|e| (e.name.as_str(), e.value.as_slice()))
    }

    /// Number of entries (including duplicates).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no entries present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut map = HeaderMap::new();
        map.insert("Content-Type", b"application/json".to_vec());
        assert_eq!(
            map.get("content-type"),
            Some(b"application/json".as_slice())
        );
        assert_eq!(
            map.get("Content-Type"),
            Some(b"application/json".as_slice())
        );
    }

    #[test]
    fn insert_replaces() {
        let mut map = HeaderMap::new();
        map.insert("x-foo", b"a".to_vec());
        map.insert("x-foo", b"b".to_vec());
        assert_eq!(map.get("x-foo"), Some(b"b".as_slice()));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn append_multi_value() {
        let mut map = HeaderMap::new();
        map.append("set-cookie", b"a=1".to_vec());
        map.append("set-cookie", b"b=2".to_vec());
        let vals: Vec<_> = map.get_all("Set-Cookie").collect();
        assert_eq!(vals, vec![b"a=1".as_slice(), b"b=2".as_slice()]);
    }

    #[test]
    fn remove() {
        let mut map = HeaderMap::new();
        map.insert("x-foo", b"bar".to_vec());
        map.remove("X-Foo");
        assert!(!map.contains("x-foo"));
    }

    #[test]
    fn get_missing_returns_none() {
        let map = HeaderMap::new();
        assert!(map.get("x-missing").is_none());
    }

    #[test]
    fn header_map_case_insensitive_get() {
        let mut map = HeaderMap::new();
        map.insert("Content-Type", b"text/html".to_vec());
        // All case variants should find the entry
        assert!(map.get("content-type").is_some());
        assert!(map.get("CONTENT-TYPE").is_some());
        assert!(map.get("Content-Type").is_some());
    }

    #[test]
    fn header_map_overwrite_keeps_single_entry() {
        let mut map = HeaderMap::new();
        map.insert("x-foo", b"first".to_vec());
        map.insert("x-foo", b"second".to_vec());
        assert_eq!(map.get("x-foo").unwrap(), b"second");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn header_map_contains_check() {
        let mut map = HeaderMap::new();
        map.insert("x-present", b"yes".to_vec());
        assert!(map.contains("x-present"));
        assert!(map.contains("X-PRESENT")); // case-insensitive
        assert!(!map.contains("x-absent"));
    }

    #[test]
    fn header_map_get_str() {
        let mut map = HeaderMap::new();
        map.insert("x-token", b"abc123".to_vec());
        assert_eq!(map.get_str("x-token"), Some("abc123"));
        assert_eq!(map.get_str("x-missing"), None);
    }

    #[test]
    fn header_map_remove_all_entries() {
        let mut map = HeaderMap::new();
        map.append("set-cookie", b"a=1".to_vec());
        map.append("set-cookie", b"b=2".to_vec());
        assert_eq!(map.len(), 2);
        map.remove("set-cookie");
        assert_eq!(map.len(), 0);
        assert!(!map.contains("set-cookie"));
    }

    #[test]
    fn header_map_iter_preserves_insertion_order() {
        let mut map = HeaderMap::new();
        map.insert("a", b"1".to_vec());
        map.insert("b", b"2".to_vec());
        map.insert("c", b"3".to_vec());
        let names: Vec<&str> = map.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn header_map_is_empty() {
        let map = HeaderMap::new();
        assert!(map.is_empty());
        let mut map2 = HeaderMap::new();
        map2.insert("x", b"y".to_vec());
        assert!(!map2.is_empty());
    }
}
