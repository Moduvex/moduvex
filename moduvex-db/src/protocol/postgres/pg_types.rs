//! PostgreSQL OID ↔ Rust type mapping (text format only).
//!
//! OID reference:
//! - `bool`      = 16
//! - `int4`      = 23
//! - `int8`      = 20
//! - `float8`    = 701
//! - `text`      = 25
//! - `timestamp` = 1114
//! - `uuid`      = 2950

use crate::error::{DbError, Result};

// ── OID constants ─────────────────────────────────────────────────────────────

pub const OID_BOOL: u32 = 16;
pub const OID_INT4: u32 = 23;
pub const OID_INT8: u32 = 20;
pub const OID_FLOAT8: u32 = 701;
pub const OID_TEXT: u32 = 25;
pub const OID_TIMESTAMP: u32 = 1114;
pub const OID_UUID: u32 = 2950;

// ── PgType ────────────────────────────────────────────────────────────────────

/// Known PostgreSQL column types (text-format only for MVP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgType {
    Bool,
    Int4,
    Int8,
    Float8,
    Text,
    Timestamp,
    Uuid,
    /// Unknown / unmapped OID — value returned as raw text bytes.
    Unknown(u32),
}

impl PgType {
    /// Resolve an OID to a `PgType`.
    pub fn from_oid(oid: u32) -> Self {
        match oid {
            OID_BOOL => PgType::Bool,
            OID_INT4 => PgType::Int4,
            OID_INT8 => PgType::Int8,
            OID_FLOAT8 => PgType::Float8,
            OID_TEXT => PgType::Text,
            OID_TIMESTAMP => PgType::Timestamp,
            OID_UUID => PgType::Uuid,
            other => PgType::Unknown(other),
        }
    }

    /// Return the OID for this type.
    pub fn oid(self) -> u32 {
        match self {
            PgType::Bool => OID_BOOL,
            PgType::Int4 => OID_INT4,
            PgType::Int8 => OID_INT8,
            PgType::Float8 => OID_FLOAT8,
            PgType::Text => OID_TEXT,
            PgType::Timestamp => OID_TIMESTAMP,
            PgType::Uuid => OID_UUID,
            PgType::Unknown(oid) => oid,
        }
    }
}

// ── Text-format decode helpers ────────────────────────────────────────────────

/// Decode a text-format column value as `i32`.
pub fn decode_i32(bytes: &[u8]) -> Result<i32> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| DbError::TypeMismatch("non-UTF-8 int4 value".into()))?;
    s.trim()
        .parse::<i32>()
        .map_err(|e| DbError::TypeMismatch(format!("cannot parse int4 '{s}': {e}")))
}

/// Decode a text-format column value as `i64`.
pub fn decode_i64(bytes: &[u8]) -> Result<i64> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| DbError::TypeMismatch("non-UTF-8 int8 value".into()))?;
    s.trim()
        .parse::<i64>()
        .map_err(|e| DbError::TypeMismatch(format!("cannot parse int8 '{s}': {e}")))
}

/// Decode a text-format column value as `f64`.
pub fn decode_f64(bytes: &[u8]) -> Result<f64> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| DbError::TypeMismatch("non-UTF-8 float8 value".into()))?;
    s.trim()
        .parse::<f64>()
        .map_err(|e| DbError::TypeMismatch(format!("cannot parse float8 '{s}': {e}")))
}

/// Decode a text-format column value as `bool`.
pub fn decode_bool(bytes: &[u8]) -> Result<bool> {
    match bytes {
        b"t" | b"true" | b"TRUE" | b"yes" | b"on" | b"1" => Ok(true),
        b"f" | b"false" | b"FALSE" | b"no" | b"off" | b"0" => Ok(false),
        other => {
            let s = String::from_utf8_lossy(other);
            Err(DbError::TypeMismatch(format!("cannot parse bool '{s}'")))
        }
    }
}

/// Decode a text-format column value as `String`.
pub fn decode_text(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| DbError::TypeMismatch("non-UTF-8 text value".into()))
}

// ── Text-format encode helpers ────────────────────────────────────────────────

/// Encode `i32` to its text-format parameter representation.
pub fn encode_i32(v: i32) -> String {
    v.to_string()
}

/// Encode `i64` to its text-format parameter representation.
pub fn encode_i64(v: i64) -> String {
    v.to_string()
}

/// Encode `f64` to its text-format parameter representation.
pub fn encode_f64(v: f64) -> String {
    v.to_string()
}

/// Encode `bool` to PostgreSQL text format (`t` / `f`).
pub fn encode_bool(v: bool) -> &'static str {
    if v {
        "t"
    } else {
        "f"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oid_roundtrip() {
        for pg_type in [
            PgType::Bool,
            PgType::Int4,
            PgType::Int8,
            PgType::Float8,
            PgType::Text,
        ] {
            assert_eq!(PgType::from_oid(pg_type.oid()), pg_type);
        }
    }

    #[test]
    fn unknown_oid_preserved() {
        let t = PgType::from_oid(99999);
        assert_eq!(t, PgType::Unknown(99999));
        assert_eq!(t.oid(), 99999);
    }

    #[test]
    fn decode_i32_valid() {
        assert_eq!(decode_i32(b"42").unwrap(), 42);
        assert_eq!(decode_i32(b"-1").unwrap(), -1);
        assert_eq!(decode_i32(b" 7 ").unwrap(), 7); // whitespace trimmed
    }

    #[test]
    fn decode_i32_invalid() {
        assert!(decode_i32(b"abc").is_err());
    }

    #[test]
    fn decode_i64_valid() {
        assert_eq!(decode_i64(b"9223372036854775807").unwrap(), i64::MAX);
    }

    #[test]
    fn decode_f64_valid() {
        let v = decode_f64(b"2.72").unwrap();
        assert!((v - 2.72).abs() < 1e-10);
    }

    #[test]
    fn decode_bool_variants() {
        assert!(decode_bool(b"t").unwrap());
        assert!(decode_bool(b"true").unwrap());
        assert!(decode_bool(b"TRUE").unwrap());
        assert!(decode_bool(b"yes").unwrap());
        assert!(!decode_bool(b"f").unwrap());
        assert!(!decode_bool(b"false").unwrap());
        assert!(!decode_bool(b"0").unwrap());
        assert!(decode_bool(b"maybe").is_err());
    }

    #[test]
    fn decode_text_valid() {
        assert_eq!(decode_text(b"hello world").unwrap(), "hello world");
    }

    #[test]
    fn encode_roundtrip_i32() {
        let s = encode_i32(42);
        assert_eq!(decode_i32(s.as_bytes()).unwrap(), 42);
    }

    #[test]
    fn encode_bool_values() {
        assert_eq!(encode_bool(true), "t");
        assert_eq!(encode_bool(false), "f");
    }
}
