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

use crate::error::{DbError, Result};
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
    /// # Errors
    /// Returns `DbError::Other` if `table` contains invalid identifier characters.
    pub fn select(table: impl Into<String>) -> Result<Self> {
        let table = table.into();
        validate_identifier(&table).map_err(DbError::Other)?;
        Ok(Self {
            table,
            ..Default::default()
        })
    }

    /// Set the columns to SELECT. Defaults to `*` if not called.
    ///
    /// # Errors
    /// Returns `DbError::Other` if any column name contains invalid identifier characters.
    pub fn columns(mut self, cols: &[&str]) -> Result<Self> {
        for col in cols {
            validate_identifier(col).map_err(DbError::Other)?;
        }
        self.columns = cols.iter().map(|s| s.to_string()).collect();
        Ok(self)
    }

    /// Add an `AND col = $n` equality condition.
    ///
    /// # Errors
    /// Returns `DbError::Other` if `col` contains invalid identifier characters.
    pub fn where_eq(mut self, col: &str, val: impl ToParam) -> Result<Self> {
        validate_identifier(col).map_err(DbError::Other)?;
        self.conditions.push((col.to_string(), val.to_param()));
        Ok(self)
    }

    /// Add an ORDER BY clause.
    ///
    /// # Errors
    /// Returns `DbError::Other` if `col` contains invalid identifier characters.
    pub fn order_by(mut self, col: &str, order: Order) -> Result<Self> {
        validate_identifier(col).map_err(DbError::Other)?;
        self.order = Some((col.to_string(), order));
        Ok(self)
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

    /// Execute this query on `conn` using the extended query protocol.
    ///
    /// Prepares the SQL (unnamed statement), binds the parameters, executes,
    /// and returns all result rows as a typed `RowSet`.
    ///
    /// This is more efficient and safer than `build_inlined()` + `conn.query()`
    /// because parameters are transmitted separately (no SQL injection risk,
    /// no manual escaping).
    ///
    /// # Example
    /// ```rust,ignore
    /// let rows = QueryBuilder::select("users")?
    ///     .where_eq("active", true)?
    ///     .execute_on(&mut conn)
    ///     .await?;
    /// ```
    pub async fn execute_on(
        self,
        conn: &mut crate::protocol::postgres::PgConnection,
    ) -> Result<super::RowSet> {
        let (sql, params) = self.build();
        let stmt = conn.prepare(&sql).await?;
        let pg_rowset = conn.execute_prepared(&stmt, &params).await?;
        Ok(super::RowSet::from(pg_rowset))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_all_no_conditions() {
        let (sql, params) = QueryBuilder::select("users").unwrap().build();
        assert_eq!(sql, "SELECT * FROM users");
        assert!(params.is_empty());
    }

    #[test]
    fn select_specific_columns() {
        let (sql, _) = QueryBuilder::select("users")
            .unwrap()
            .columns(&["id", "name"])
            .unwrap()
            .build();
        assert_eq!(sql, "SELECT id, name FROM users");
    }

    #[test]
    fn select_with_where_eq() {
        let (sql, params) = QueryBuilder::select("users")
            .unwrap()
            .columns(&["id"])
            .unwrap()
            .where_eq("active", true)
            .unwrap()
            .build();
        assert!(sql.contains("WHERE active = $1"));
        assert_eq!(params, vec![Param::Bool(true)]);
    }

    #[test]
    fn select_multiple_conditions() {
        let (sql, params) = QueryBuilder::select("users")
            .unwrap()
            .where_eq("id", 42i32)
            .unwrap()
            .where_eq("name", "Alice")
            .unwrap()
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
            .unwrap()
            .order_by("created_at", Order::Desc)
            .unwrap()
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
            .unwrap()
            .where_eq("id", 7i32)
            .unwrap()
            .where_eq("name", "Bob")
            .unwrap()
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

    // ── Additional builder tests ───────────────────────────────────────────────

    #[test]
    fn builder_invalid_table_name_rejected() {
        let result = QueryBuilder::select("users; DROP TABLE users");
        assert!(result.is_err());
    }

    #[test]
    fn builder_table_name_with_spaces_rejected() {
        assert!(QueryBuilder::select("my table").is_err());
    }

    #[test]
    fn builder_invalid_column_name_rejected() {
        let result = QueryBuilder::select("users")
            .unwrap()
            .columns(&["id", "name; DROP TABLE"]);
        assert!(result.is_err());
    }

    #[test]
    fn builder_schema_dot_table_allowed() {
        let (sql, _) = QueryBuilder::select("public.users").unwrap().build();
        assert!(sql.contains("FROM public.users"));
    }

    #[test]
    fn builder_limit_without_offset() {
        let (sql, _) = QueryBuilder::select("t").unwrap().limit(5).build();
        assert!(sql.contains("LIMIT 5"));
        assert!(!sql.contains("OFFSET"));
    }

    #[test]
    fn builder_offset_without_limit() {
        let (sql, _) = QueryBuilder::select("t").unwrap().offset(10).build();
        assert!(sql.contains("OFFSET 10"));
        assert!(!sql.contains("LIMIT"));
    }

    #[test]
    fn builder_zero_limit() {
        let (sql, _) = QueryBuilder::select("t").unwrap().limit(0).build();
        assert!(sql.contains("LIMIT 0"));
    }

    #[test]
    fn builder_where_eq_bool_false() {
        let (sql, params) = QueryBuilder::select("users")
            .unwrap()
            .where_eq("active", false)
            .unwrap()
            .build();
        assert!(sql.contains("WHERE active = $1"));
        assert_eq!(params[0], Param::Bool(false));
    }

    #[test]
    fn builder_where_eq_negative_int() {
        let (sql, params) = QueryBuilder::select("t")
            .unwrap()
            .where_eq("score", -5i32)
            .unwrap()
            .build();
        assert!(sql.contains("score = $1"));
        assert_eq!(params[0], Param::Int4(-5));
    }

    #[test]
    fn builder_all_clauses_together() {
        let (sql, params) = QueryBuilder::select("orders")
            .unwrap()
            .columns(&["id", "total"])
            .unwrap()
            .where_eq("user_id", 1i32)
            .unwrap()
            .where_eq("status", "active")
            .unwrap()
            .order_by("created_at", Order::Desc)
            .unwrap()
            .limit(20)
            .offset(40)
            .build();
        assert!(sql.starts_with("SELECT id, total FROM orders"));
        assert!(sql.contains("WHERE user_id = $1 AND status = $2"));
        assert!(sql.contains("ORDER BY created_at DESC"));
        assert!(sql.contains("LIMIT 20"));
        assert!(sql.contains("OFFSET 40"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn builder_inlined_no_placeholders_remain() {
        let sql = QueryBuilder::select("users")
            .unwrap()
            .where_eq("id", 42i32)
            .unwrap()
            .where_eq("name", "Alice")
            .unwrap()
            .build_inlined()
            .unwrap();
        assert!(!sql.contains('$'));
    }

    #[test]
    fn builder_order_asc_generates_asc() {
        let (sql, _) = QueryBuilder::select("t")
            .unwrap()
            .order_by("col", Order::Asc)
            .unwrap()
            .build();
        assert!(sql.contains("ORDER BY col ASC"));
    }

    #[test]
    fn builder_invalid_order_col_rejected() {
        let result = QueryBuilder::select("t")
            .unwrap()
            .order_by("col; DROP TABLE", Order::Asc);
        assert!(result.is_err());
    }

    #[test]
    fn builder_invalid_where_col_rejected() {
        let result = QueryBuilder::select("t")
            .unwrap()
            .where_eq("id OR 1=1", 1i32);
        assert!(result.is_err());
    }

    #[test]
    fn builder_select_star_default() {
        let (sql, _) = QueryBuilder::select("users").unwrap().build();
        assert!(sql.starts_with("SELECT * FROM users"));
    }

    #[test]
    fn builder_empty_table_rejected() {
        assert!(QueryBuilder::select("").is_err());
    }

    #[test]
    fn builder_params_indexed_correctly() {
        let (sql, params) = QueryBuilder::select("t")
            .unwrap()
            .where_eq("a", 1i32)
            .unwrap()
            .where_eq("b", 2i32)
            .unwrap()
            .where_eq("c", 3i32)
            .unwrap()
            .build();
        assert!(sql.contains("a = $1"));
        assert!(sql.contains("b = $2"));
        assert!(sql.contains("c = $3"));
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn builder_no_conditions_no_where_clause() {
        let (sql, _) = QueryBuilder::select("t").unwrap().build();
        assert!(!sql.contains("WHERE"));
    }
}
