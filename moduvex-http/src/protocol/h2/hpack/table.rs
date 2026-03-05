//! HPACK header table — static (RFC 7541 Appendix A) + dynamic (FIFO eviction).

#![allow(dead_code)]

use std::collections::VecDeque;

// ── Static Table (RFC 7541 Appendix A) ────────────────────────────────────────

/// All 61 static table entries. Index is 1-based (entry 0 unused per spec).
pub const STATIC_TABLE: &[(&str, &str)] = &[
    ("", ""),                               // 0 — unused placeholder
    (":authority", ""),                     // 1
    (":method", "GET"),                     // 2
    (":method", "POST"),                    // 3
    (":path", "/"),                         // 4
    (":path", "/index.html"),               // 5
    (":scheme", "http"),                    // 6
    (":scheme", "https"),                   // 7
    (":status", "200"),                     // 8
    (":status", "204"),                     // 9
    (":status", "206"),                     // 10
    (":status", "304"),                     // 11
    (":status", "400"),                     // 12
    (":status", "404"),                     // 13
    (":status", "500"),                     // 14
    ("accept-charset", ""),                 // 15
    ("accept-encoding", "gzip, deflate"),   // 16
    ("accept-language", ""),               // 17
    ("accept-ranges", ""),                  // 18
    ("accept", ""),                         // 19
    ("access-control-allow-origin", ""),   // 20
    ("age", ""),                            // 21
    ("allow", ""),                          // 22
    ("authorization", ""),                  // 23
    ("cache-control", ""),                  // 24
    ("content-disposition", ""),           // 25
    ("content-encoding", ""),              // 26
    ("content-language", ""),              // 27
    ("content-length", ""),                // 28
    ("content-location", ""),              // 29
    ("content-range", ""),                 // 30
    ("content-type", ""),                  // 31
    ("cookie", ""),                        // 32
    ("date", ""),                          // 33
    ("etag", ""),                          // 34
    ("expect", ""),                        // 35
    ("expires", ""),                       // 36
    ("from", ""),                          // 37
    ("host", ""),                          // 38
    ("if-match", ""),                      // 39
    ("if-modified-since", ""),             // 40
    ("if-none-match", ""),                 // 41
    ("if-range", ""),                      // 42
    ("if-unmodified-since", ""),           // 43
    ("last-modified", ""),                 // 44
    ("link", ""),                          // 45
    ("location", ""),                      // 46
    ("max-forwards", ""),                  // 47
    ("proxy-authenticate", ""),            // 48
    ("proxy-authorization", ""),           // 49
    ("range", ""),                         // 50
    ("referer", ""),                       // 51
    ("refresh", ""),                       // 52
    ("retry-after", ""),                   // 53
    ("server", ""),                        // 54
    ("set-cookie", ""),                    // 55
    ("strict-transport-security", ""),     // 56
    ("transfer-encoding", ""),             // 57
    ("user-agent", ""),                    // 58
    ("vary", ""),                          // 59
    ("via", ""),                           // 60
    ("www-authenticate", ""),              // 61
];

// ── Dynamic Table ─────────────────────────────────────────────────────────────

/// Dynamic table with FIFO eviction per RFC 7541 Section 4.
pub struct DynamicTable {
    entries: VecDeque<(Vec<u8>, Vec<u8>)>,
    current_size: usize,
    max_size: usize,
}

impl DynamicTable {
    /// Create a dynamic table with the given maximum size (in octets).
    pub fn new(max_size: usize) -> Self {
        Self { entries: VecDeque::new(), current_size: 0, max_size }
    }

    /// Look up an entry by dynamic-table-local index (0-based, newest first).
    pub fn lookup(&self, index: usize) -> Option<(&[u8], &[u8])> {
        self.entries.get(index).map(|(n, v)| (n.as_slice(), v.as_slice()))
    }

    /// Combined lookup: index 1–61 = static table, 62+ = dynamic table.
    pub fn get(&self, index: usize) -> Option<(&[u8], &[u8])> {
        if index == 0 {
            return None;
        }
        if index <= 61 {
            let (n, v) = STATIC_TABLE[index];
            return Some((n.as_bytes(), v.as_bytes()));
        }
        // dynamic index is 0-based from the front (newest = index 62)
        self.lookup(index - 62)
    }

    /// Insert a new entry at the front; evict from the back as needed.
    pub fn insert(&mut self, name: Vec<u8>, value: Vec<u8>) {
        let sz = Self::entry_size(&name, &value);
        // If larger than the entire max size, evict everything and don't insert.
        if sz > self.max_size {
            self.entries.clear();
            self.current_size = 0;
            return;
        }
        self.current_size += sz;
        self.entries.push_front((name, value));
        self.evict();
    }

    /// Resize the table; evict entries if the new max is smaller.
    pub fn resize(&mut self, new_max: usize) {
        self.max_size = new_max;
        self.evict();
    }

    /// Evict oldest entries (back of deque) until within max_size.
    fn evict(&mut self) {
        while self.current_size > self.max_size {
            if let Some((n, v)) = self.entries.pop_back() {
                self.current_size -= Self::entry_size(&n, &v);
            } else {
                break;
            }
        }
    }

    /// RFC 7541 Section 4.1: entry size = name.len + value.len + 32.
    fn entry_size(name: &[u8], value: &[u8]) -> usize {
        name.len() + value.len() + 32
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_table_entry_count() {
        // Placeholder at index 0 + 61 real entries = 62 total.
        assert_eq!(STATIC_TABLE.len(), 62);
    }

    #[test]
    fn static_lookup_method_get() {
        let t = DynamicTable::new(4096);
        let (n, v) = t.get(2).unwrap();
        assert_eq!(n, b":method");
        assert_eq!(v, b"GET");
    }

    #[test]
    fn static_lookup_method_post() {
        let t = DynamicTable::new(4096);
        let (n, v) = t.get(3).unwrap();
        assert_eq!(n, b":method");
        assert_eq!(v, b"POST");
    }

    #[test]
    fn dynamic_insert_and_lookup() {
        let mut t = DynamicTable::new(4096);
        t.insert(b"x-custom".to_vec(), b"hello".to_vec());
        let (n, v) = t.get(62).unwrap();
        assert_eq!(n, b"x-custom");
        assert_eq!(v, b"hello");
    }

    #[test]
    fn dynamic_eviction_on_overflow() {
        // Entry size = 3 + 3 + 32 = 38; max 50 allows one entry.
        let mut t = DynamicTable::new(50);
        t.insert(b"aaa".to_vec(), b"bbb".to_vec());
        t.insert(b"ccc".to_vec(), b"ddd".to_vec());
        // Only the newest should remain.
        assert_eq!(t.entries.len(), 1);
        let (n, _) = t.get(62).unwrap();
        assert_eq!(n, b"ccc");
    }

    #[test]
    fn dynamic_too_large_clears_table() {
        let mut t = DynamicTable::new(10);
        t.insert(b"big_name_here".to_vec(), b"big_value".to_vec());
        assert!(t.entries.is_empty());
    }

    #[test]
    fn zero_index_returns_none() {
        let t = DynamicTable::new(4096);
        assert!(t.get(0).is_none());
    }
}
