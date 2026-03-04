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
}
