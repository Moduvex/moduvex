//! Query layer — `Row`, `Column`, `RowSet`, re-exports of builder and param.
//!
//! `Row` and `RowSet` are thin wrappers that add typed accessor methods over
//! the raw `PgRow`/`PgRowSet` from the protocol layer.

pub mod builder;
pub mod param;

use crate::error::{DbError, Result};
use crate::protocol::postgres::pg_types::{
    decode_bool, decode_f64, decode_i32, decode_i64, decode_text, PgType,
};
use crate::protocol::postgres::{PgColumn, PgRow, PgRowSet};

// ── Column ────────────────────────────────────────────────────────────────────

/// Metadata for a single result column.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub pg_type: PgType,
}

impl From<PgColumn> for Column {
    fn from(c: PgColumn) -> Self {
        Self {
            name: c.name,
            pg_type: PgType::from_oid(c.type_oid),
        }
    }
}

// ── Row ───────────────────────────────────────────────────────────────────────

/// A single data row from a query result.
///
/// Field values are raw text bytes from PostgreSQL; use `get::<T>()` to decode.
#[derive(Debug, Clone)]
pub struct Row {
    pub(crate) columns: Vec<Column>,
    pub(crate) fields: Vec<Option<Vec<u8>>>,
}

impl From<PgRow> for Row {
    fn from(r: PgRow) -> Self {
        let columns = r.columns.into_iter().map(Column::from).collect();
        Self {
            columns,
            fields: r.fields,
        }
    }
}

impl Row {
    /// Get the number of columns in this row.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// True if the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// Get a typed value by column name.
    ///
    /// Returns `DbError::NullValue` if the column is SQL NULL.
    /// Returns `DbError::TypeMismatch` if parsing fails.
    pub fn get<T: FromRow>(&self, col: &str) -> Result<T> {
        let idx = self.column_index(col)?;
        match &self.fields[idx] {
            None => Err(DbError::NullValue {
                column: col.to_string(),
            }),
            Some(bytes) => T::from_bytes(bytes),
        }
    }

    /// Get a typed value by column index (0-based).
    pub fn get_by_index<T: FromRow>(&self, idx: usize) -> Result<T> {
        if idx >= self.fields.len() {
            return Err(DbError::Other(format!("column index {idx} out of range")));
        }
        match &self.fields[idx] {
            None => Err(DbError::NullValue {
                column: self
                    .columns
                    .get(idx)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| idx.to_string()),
            }),
            Some(bytes) => T::from_bytes(bytes),
        }
    }

    /// Get an optional typed value (returns `Ok(None)` for SQL NULL).
    pub fn get_opt<T: FromRow>(&self, col: &str) -> Result<Option<T>> {
        let idx = self.column_index(col)?;
        match &self.fields[idx] {
            None => Ok(None),
            Some(bytes) => T::from_bytes(bytes).map(Some),
        }
    }

    /// Return the raw bytes for a column (None = SQL NULL).
    pub fn raw(&self, col: &str) -> Result<Option<&[u8]>> {
        let idx = self.column_index(col)?;
        Ok(self.fields[idx].as_deref())
    }

    fn column_index(&self, col: &str) -> Result<usize> {
        self.columns
            .iter()
            .position(|c| c.name == col)
            .ok_or_else(|| DbError::Other(format!("column '{col}' not found in result set")))
    }
}

// ── RowSet ────────────────────────────────────────────────────────────────────

/// Complete result set from a query: column metadata + all rows.
#[derive(Debug)]
pub struct RowSet {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
}

impl From<PgRowSet> for RowSet {
    fn from(rs: PgRowSet) -> Self {
        let columns: Vec<Column> = rs.columns.into_iter().map(Column::from).collect();
        let rows = rs.rows.into_iter().map(Row::from).collect();
        Self { columns, rows }
    }
}

impl RowSet {
    /// Number of rows returned.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// True if no rows were returned.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Iterate over rows.
    pub fn iter(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter()
    }
}

// ── FromRow ───────────────────────────────────────────────────────────────────

/// Implemented by types that can be decoded from a PostgreSQL text-format column.
pub trait FromRow: Sized {
    fn from_bytes(bytes: &[u8]) -> Result<Self>;
}

impl FromRow for i32 {
    fn from_bytes(b: &[u8]) -> Result<Self> {
        decode_i32(b)
    }
}
impl FromRow for i64 {
    fn from_bytes(b: &[u8]) -> Result<Self> {
        decode_i64(b)
    }
}
impl FromRow for f64 {
    fn from_bytes(b: &[u8]) -> Result<Self> {
        decode_f64(b)
    }
}
impl FromRow for bool {
    fn from_bytes(b: &[u8]) -> Result<Self> {
        decode_bool(b)
    }
}
impl FromRow for String {
    fn from_bytes(b: &[u8]) -> Result<Self> {
        decode_text(b)
    }
}
impl FromRow for Vec<u8> {
    fn from_bytes(b: &[u8]) -> Result<Self> {
        Ok(b.to_vec())
    }
}

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use builder::{Order, QueryBuilder};
pub use param::{substitute_params, Param, ToParam};

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(values: &[(&str, Option<&[u8]>)]) -> Row {
        let columns: Vec<Column> = values
            .iter()
            .map(|(name, _)| Column {
                name: name.to_string(),
                pg_type: PgType::Text,
            })
            .collect();
        let fields: Vec<Option<Vec<u8>>> =
            values.iter().map(|(_, v)| v.map(|b| b.to_vec())).collect();
        Row { columns, fields }
    }

    #[test]
    fn row_get_string() {
        let row = make_row(&[("name", Some(b"Alice"))]);
        assert_eq!(row.get::<String>("name").unwrap(), "Alice");
    }

    #[test]
    fn row_get_i32() {
        let row = make_row(&[("id", Some(b"42"))]);
        assert_eq!(row.get::<i32>("id").unwrap(), 42);
    }

    #[test]
    fn row_get_null_returns_error() {
        let row = make_row(&[("email", None)]);
        assert!(matches!(
            row.get::<String>("email"),
            Err(DbError::NullValue { .. })
        ));
    }

    #[test]
    fn row_get_opt_null_returns_ok_none() {
        let row = make_row(&[("email", None)]);
        assert_eq!(row.get_opt::<String>("email").unwrap(), None);
    }

    #[test]
    fn row_get_missing_column_returns_err() {
        let row = make_row(&[("id", Some(b"1"))]);
        assert!(row.get::<String>("missing").is_err());
    }

    #[test]
    fn row_get_bool() {
        let row = make_row(&[("active", Some(b"t"))]);
        assert!(row.get::<bool>("active").unwrap());
    }

    #[test]
    fn rowset_len_and_iter() {
        let rowset = RowSet {
            columns: vec![Column {
                name: "x".into(),
                pg_type: PgType::Int4,
            }],
            rows: vec![
                make_row(&[("x", Some(b"1"))]),
                make_row(&[("x", Some(b"2"))]),
            ],
        };
        assert_eq!(rowset.len(), 2);
        let vals: Vec<i32> = rowset.iter().map(|r| r.get::<i32>("x").unwrap()).collect();
        assert_eq!(vals, vec![1, 2]);
    }
}
