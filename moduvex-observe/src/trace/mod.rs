//! Distributed tracing: TraceId, SpanId, Span lifecycle, and task-local context.

pub mod context;
pub mod span;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 128-bit trace identifier (high + low 64-bit halves).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId(pub u64, pub u64);

/// 64-bit span identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId(pub u64);

// ── Simple PRNG for ID generation ──
// Uses a per-thread counter mixed with timestamp — not cryptographic,
// but sufficient for trace IDs.

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// Seed value derived from system time.
fn time_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// Mix bits for better distribution (splitmix64-style).
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    x
}

impl TraceId {
    /// Generate a new pseudo-random TraceId.
    pub fn generate() -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let hi = mix(time_seed().wrapping_add(seq));
        let lo = mix(hi.wrapping_add(seq));
        Self(hi, lo)
    }
}

impl std::fmt::Display for TraceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}{:016x}", self.0, self.1)
    }
}

impl SpanId {
    /// Generate a new pseudo-random SpanId.
    pub fn generate() -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(mix(time_seed().wrapping_add(seq)))
    }
}

impl std::fmt::Display for SpanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_id_unique() {
        let a = TraceId::generate();
        let b = TraceId::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn span_id_unique() {
        let a = SpanId::generate();
        let b = SpanId::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn trace_id_display_is_32_hex_chars() {
        let id = TraceId::generate();
        let s = id.to_string();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn span_id_display_is_16_hex_chars() {
        let id = SpanId::generate();
        let s = id.to_string();
        assert_eq!(s.len(), 16);
    }

    #[test]
    fn trace_id_all_hex_chars() {
        let id = TraceId::generate();
        let s = id.to_string();
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn span_id_all_hex_chars() {
        let id = SpanId::generate();
        let s = id.to_string();
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn trace_id_equality_with_same_values() {
        let id = TraceId(0xdeadbeef, 0xcafebabe);
        let id2 = TraceId(0xdeadbeef, 0xcafebabe);
        assert_eq!(id, id2);
    }

    #[test]
    fn span_id_equality_with_same_value() {
        let id = SpanId(42);
        let id2 = SpanId(42);
        assert_eq!(id, id2);
    }

    #[test]
    fn trace_id_inequality_different_high() {
        let a = TraceId(1, 0);
        let b = TraceId(2, 0);
        assert_ne!(a, b);
    }

    #[test]
    fn trace_id_inequality_different_low() {
        let a = TraceId(0, 1);
        let b = TraceId(0, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn bulk_trace_ids_all_unique() {
        use std::collections::HashSet;
        let ids: HashSet<_> = (0..100).map(|_| TraceId::generate()).collect();
        assert_eq!(ids.len(), 100);
    }

    #[test]
    fn bulk_span_ids_all_unique() {
        use std::collections::HashSet;
        let ids: HashSet<_> = (0..100).map(|_| SpanId::generate()).collect();
        assert_eq!(ids.len(), 100);
    }

    #[test]
    fn trace_id_debug_format() {
        let id = TraceId(1, 2);
        let s = format!("{id:?}");
        assert!(s.contains("TraceId"));
    }

    #[test]
    fn span_id_debug_format() {
        let id = SpanId(99);
        let s = format!("{id:?}");
        assert!(s.contains("SpanId"));
    }
}
