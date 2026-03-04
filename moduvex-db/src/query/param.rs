//! Parameterized query values — `Param` enum + `ToParam` trait.
//!
//! Params are sent to PostgreSQL as text-format values in the query string
//! using `$1`, `$2`, ... placeholders. Proper escaping prevents SQL injection.

use crate::error::{DbError, Result};

// ── Param ─────────────────────────────────────────────────────────────────────

/// A typed query parameter value.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Null,
    Bool(bool),
    Int4(i32),
    Int8(i64),
    Float8(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl Param {
    /// Encode this parameter as a PostgreSQL text-format string.
    ///
    /// Returns `None` for `Null` (will be rendered as `NULL` literal in SQL).
    pub fn encode_text(&self) -> Option<String> {
        match self {
            Param::Null       => None,
            Param::Bool(b)    => Some(if *b { "t".into() } else { "f".into() }),
            Param::Int4(n)    => Some(n.to_string()),
            Param::Int8(n)    => Some(n.to_string()),
            Param::Float8(f)  => Some(f.to_string()),
            Param::Text(s)    => Some(s.clone()),
            Param::Bytes(b)   => Some(hex_encode(b)),
        }
    }

    /// Inline this parameter into a SQL literal safe for simple query protocol.
    ///
    /// Text values are single-quoted with internal single-quotes escaped as `''`.
    /// This is used by `QueryBuilder::build_with_params` to produce a single
    /// SQL string (avoiding the extended query protocol for MVP).
    pub fn to_sql_literal(&self) -> String {
        match self {
            Param::Null      => "NULL".into(),
            Param::Bool(b)   => if *b { "TRUE".into() } else { "FALSE".into() },
            Param::Int4(n)   => n.to_string(),
            Param::Int8(n)   => n.to_string(),
            Param::Float8(f) => f.to_string(),
            Param::Text(s)   => format!("'{}'", s.replace('\'', "''")),
            Param::Bytes(b)  => format!("'\\x{}'", hex_encode(b)),
        }
    }
}

/// Encode bytes as a lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── ToParam ───────────────────────────────────────────────────────────────────

/// Implemented by types that can be converted to a `Param`.
pub trait ToParam {
    fn to_param(&self) -> Param;
}

impl ToParam for bool    { fn to_param(&self) -> Param { Param::Bool(*self) } }
impl ToParam for i32     { fn to_param(&self) -> Param { Param::Int4(*self) } }
impl ToParam for i64     { fn to_param(&self) -> Param { Param::Int8(*self) } }
impl ToParam for f64     { fn to_param(&self) -> Param { Param::Float8(*self) } }
impl ToParam for String  { fn to_param(&self) -> Param { Param::Text(self.clone()) } }
impl ToParam for str     { fn to_param(&self) -> Param { Param::Text(self.to_owned()) } }
impl ToParam for Vec<u8> { fn to_param(&self) -> Param { Param::Bytes(self.clone()) } }
impl ToParam for &str    { fn to_param(&self) -> Param { Param::Text((*self).to_owned()) } }

impl<T: ToParam> ToParam for Option<T> {
    fn to_param(&self) -> Param {
        match self {
            Some(v) => v.to_param(),
            None    => Param::Null,
        }
    }
}

// ── Substitute params into SQL ────────────────────────────────────────────────

/// Replace `$1`, `$2`, … placeholders in `sql` with inlined SQL literals.
///
/// # Errors
/// Returns `Err` if a placeholder index is out of range (1-based).
pub fn substitute_params(sql: &str, params: &[Param]) -> Result<String> {
    let mut result = String::with_capacity(sql.len() + params.len() * 4);
    let bytes = sql.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            // Collect the full number (may be multi-digit: $10, $42, …)
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let idx_str = &sql[start..end];
            let idx: usize = idx_str.parse().unwrap(); // safe: only digits
            if idx == 0 || idx > params.len() {
                return Err(DbError::Other(format!(
                    "parameter ${idx} out of range (have {} params)", params.len()
                )));
            }
            result.push_str(&params[idx - 1].to_sql_literal());
            i = end;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    Ok(result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_encode_text() {
        assert_eq!(Param::Bool(true).encode_text(),  Some("t".into()));
        assert_eq!(Param::Bool(false).encode_text(), Some("f".into()));
        assert_eq!(Param::Int4(42).encode_text(),    Some("42".into()));
        assert_eq!(Param::Int8(-1).encode_text(),    Some("-1".into()));
        assert_eq!(Param::Float8(3.14).encode_text(),Some("3.14".into()));
        assert_eq!(Param::Text("hi".into()).encode_text(), Some("hi".into()));
        assert_eq!(Param::Null.encode_text(), None);
    }

    #[test]
    fn param_sql_literal_text_escaping() {
        let p = Param::Text("it's a test".into());
        assert_eq!(p.to_sql_literal(), "'it''s a test'");
    }

    #[test]
    fn param_sql_literal_null() {
        assert_eq!(Param::Null.to_sql_literal(), "NULL");
    }

    #[test]
    fn param_sql_literal_bool() {
        assert_eq!(Param::Bool(true).to_sql_literal(), "TRUE");
        assert_eq!(Param::Bool(false).to_sql_literal(), "FALSE");
    }

    #[test]
    fn to_param_implementations() {
        assert_eq!(42i32.to_param(), Param::Int4(42));
        assert_eq!((-1i64).to_param(), Param::Int8(-1));
        assert_eq!(3.14f64.to_param(), Param::Float8(3.14));
        assert_eq!("hello".to_param(), Param::Text("hello".into()));
        assert_eq!(true.to_param(), Param::Bool(true));
        let opt: Option<i32> = None;
        assert_eq!(opt.to_param(), Param::Null);
        let opt2: Option<i32> = Some(7);
        assert_eq!(opt2.to_param(), Param::Int4(7));
    }

    #[test]
    fn substitute_params_basic() {
        let sql = "SELECT * FROM users WHERE id = $1 AND active = $2";
        let params = vec![Param::Int4(5), Param::Bool(true)];
        let result = substitute_params(sql, &params).unwrap();
        assert_eq!(result, "SELECT * FROM users WHERE id = 5 AND active = TRUE");
    }

    #[test]
    fn substitute_params_text_quoted() {
        let sql = "INSERT INTO t VALUES ($1)";
        let params = vec![Param::Text("O'Brien".into())];
        let result = substitute_params(sql, &params).unwrap();
        assert_eq!(result, "INSERT INTO t VALUES ('O''Brien')");
    }

    #[test]
    fn substitute_params_null() {
        let params = vec![Param::Null];
        let result = substitute_params("SELECT $1", &params).unwrap();
        assert_eq!(result, "SELECT NULL");
    }

    #[test]
    fn substitute_params_out_of_range_returns_err() {
        let params = vec![Param::Int4(1)];
        assert!(substitute_params("SELECT $2", &params).is_err());
    }

    #[test]
    fn hex_encode_bytes() {
        let p = Param::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(p.to_sql_literal().contains("deadbeef"));
    }
}
