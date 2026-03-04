//! Fluent `QueryBuilder` for constructing parameterized SQL queries.
//!
//! Usage:
//! ```rust,ignore
//! let (sql, params) = QueryBuilder::select("users")
//!     .columns(&["id", "name"])
//!     .where_eq("active", true)
//!     .order_by("created_at", Order::Desc)
//!     .limit(10)
//!     .build();
//! ```

use crate::error::Result;
use crate::query::param::{substitute_params, Param, ToParam};

// ── Order ─────────────────────────────────────────────────────────────────────

/// Sort direction for `ORDER BY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

impl Order {
    fn as_sql(&self) -> &'static str {
        match self {
            Order::Asc => "ASC",
            Order::Desc => "DESC",
        }
    }
}

// ── Identifier validation ─────────────────────────────────────────────────────

/// Validate a SQL identifier (table name, column name) to prevent injection.
/// Allows alphanumeric, underscore, dot (schema.table), and rejects everything else.
fn validate_identifier(ident: &str) -> std::result::Result<(), String> {
    if ident.is_empty() {
        return Err("SQL identifier cannot be empty".to_string());
    }
    if ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        Ok(())
    } else {
        Err(format!("invalid SQL identifier: {ident:?}"))
    }
}

// ── QueryBuilder ──────────────────────────────────────────────────────────────

/// Fluent builder for SELECT queries (MVP scope).
///
/// Builds a parameterized `(sql, params)` pair. Call `build()` or
/// `build_inlined()` to get the final SQL string.
#[derive(Debug, Default)]
pub struct QueryBuilder {
    table: String,
    columns: Vec<String>,
    conditions: Vec<(String, Param)>,
    order: Option<(String, Order)>,
    limit: Option<usize>,
    offset: Option<usize>,
}

impl QueryBuilder {
    /// Start a SELECT query against `table`.
    ///
    /// # Panics
    /// Panics if `table` contains invalid identifier characters.
    pub fn select(table: impl Into<String>) -> Self {
        let table = table.into();
        validate_identifier(&table).expect("invalid table name");
        Self {
            table,
            ..Default::default()
        }
    }

    /// Set the columns to SELECT. Defaults to `*` if not called.
    ///
    /// # Panics
    /// Panics if any column name contains invalid identifier characters.
    pub fn columns(mut self, cols: &[&str]) -> Self {
        for col in cols {
            validate_identifier(col).expect("invalid column name");
        }
        self.columns = cols.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Add an `AND col = $n` equality condition.
    ///
    /// # Panics
    /// Panics if `col` contains invalid identifier characters.
    pub fn where_eq(mut self, col: &str, val: impl ToParam) -> Self {
        validate_identifier(col).expect("invalid column name in where_eq");
        self.conditions.push((col.to_string(), val.to_param()));
        self
    }

    /// Add an ORDER BY clause.
    ///
    /// # Panics
    /// Panics if `col` contains invalid identifier characters.
    pub fn order_by(mut self, col: &str, order: Order) -> Self {
        validate_identifier(col).expect("invalid column name in order_by");
        self.order = Some((col.to_string(), order));
        self
    }

    /// Add a LIMIT clause.
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Add an OFFSET clause.
    pub fn offset(mut self, n: usize) -> Self {
        self.offset = Some(n);
        self
    }

    /// Build into `(sql_with_placeholders, params)`.
    ///
    /// The SQL uses `$1`, `$2`, … placeholders for parameterized execution.
    pub fn build(self) -> (String, Vec<Param>) {
        let col_list = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns.join(", ")
        };

        let mut sql = format!("SELECT {col_list} FROM {}", self.table);
        let mut params: Vec<Param> = Vec::new();

        if !self.conditions.is_empty() {
            let where_parts: Vec<String> = self
                .conditions
                .into_iter()
                .enumerate()
                .map(|(i, (col, val))| {
                    params.push(val);
                    format!("{col} = ${}", i + 1)
                })
                .collect();
            sql.push_str(" WHERE ");
            sql.push_str(&where_parts.join(" AND "));
        }

        if let Some((col, ord)) = self.order {
            sql.push_str(&format!(" ORDER BY {col} {}", ord.as_sql()));
        }
        if let Some(lim) = self.limit {
            sql.push_str(&format!(" LIMIT {lim}"));
        }
        if let Some(off) = self.offset {
            sql.push_str(&format!(" OFFSET {off}"));
        }

        (sql, params)
    }

    /// Build with params inlined into the SQL (for simple query protocol).
    ///
    /// Returns a single SQL string with parameter values substituted.
    pub fn build_inlined(self) -> Result<String> {
        let (sql, params) = self.build();
        substitute_params(&sql, &params)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_all_no_conditions() {
        let (sql, params) = QueryBuilder::select("users").build();
        assert_eq!(sql, "SELECT * FROM users");
        assert!(params.is_empty());
    }

    #[test]
    fn select_specific_columns() {
        let (sql, _) = QueryBuilder::select("users")
            .columns(&["id", "name"])
            .build();
        assert_eq!(sql, "SELECT id, name FROM users");
    }

    #[test]
    fn select_with_where_eq() {
        let (sql, params) = QueryBuilder::select("users")
            .columns(&["id"])
            .where_eq("active", true)
            .build();
        assert!(sql.contains("WHERE active = $1"));
        assert_eq!(params, vec![Param::Bool(true)]);
    }

    #[test]
    fn select_multiple_conditions() {
        let (sql, params) = QueryBuilder::select("users")
            .where_eq("id", 42i32)
            .where_eq("name", "Alice")
            .build();
        assert!(sql.contains("id = $1"));
        assert!(sql.contains("name = $2"));
        assert!(sql.contains("AND"));
        assert_eq!(params[0], Param::Int4(42));
        assert_eq!(params[1], Param::Text("Alice".into()));
    }

    #[test]
    fn select_with_order_limit_offset() {
        let (sql, _) = QueryBuilder::select("posts")
            .order_by("created_at", Order::Desc)
            .limit(10)
            .offset(20)
            .build();
        assert!(sql.contains("ORDER BY created_at DESC"));
        assert!(sql.contains("LIMIT 10"));
        assert!(sql.contains("OFFSET 20"));
    }

    #[test]
    fn build_inlined_substitutes_params() {
        let sql = QueryBuilder::select("users")
            .where_eq("id", 7i32)
            .where_eq("name", "Bob")
            .build_inlined()
            .unwrap();
        assert!(sql.contains("id = 7"));
        assert!(sql.contains("name = 'Bob'"));
        // No placeholders remain
        assert!(!sql.contains('$'));
    }

    #[test]
    fn order_asc_desc_sql() {
        assert_eq!(Order::Asc.as_sql(), "ASC");
        assert_eq!(Order::Desc.as_sql(), "DESC");
    }
}
