//! PostgreSQL extended query protocol — prepared statements.
//!
//! The extended query protocol separates parsing from execution, enabling:
//!   - Efficient re-execution of the same query with different parameters
//!   - Type-safe parameter binding without string interpolation
//!   - Server-side query plan caching
//!
//! Flow:
//!   1. `prepare(sql)` — Parse + Describe + Sync → server validates SQL and returns type info
//!   2. `execute_prepared(stmt, params)` — Bind + Execute + Sync → fetch rows
//!
//! Text format is used for both parameters and results (simplest to implement;
//! binary format is left as a future optimization).

use crate::error::{DbError, Result};
use crate::protocol::postgres::codec::{
    decode_backend, encode_bind, encode_close, encode_describe, encode_execute, encode_parse,
    encode_sync, BackendMessage, ColumnDesc, MSG_BIND, MSG_CLOSE, MSG_DESCRIBE, MSG_EXECUTE,
    MSG_PARSE, MSG_SYNC,
};
use crate::protocol::postgres::wire::{read_backend_message, write_frontend_message};
use crate::protocol::postgres::{PgColumn, PgRow, PgRowSet};
use crate::query::param::Param;

use moduvex_runtime::net::TcpStream;

// ── OID mapping ───────────────────────────────────────────────────────────────

/// Map a `Param` variant to its corresponding PostgreSQL OID for type hinting.
///
/// Passing OID=0 (unspecified) lets PostgreSQL infer the type; we use explicit
/// OIDs for better error messages and to avoid ambiguity.
pub fn param_to_oid(param: &Param) -> u32 {
    match param {
        Param::Null => 0,        // unspecified — server infers
        Param::Bool(_) => 16,   // bool
        Param::Int4(_) => 23,   // int4
        Param::Int8(_) => 20,   // int8
        Param::Float8(_) => 701, // float8
        Param::Text(_) => 25,   // text
        Param::Bytes(_) => 17,  // bytea
    }
}

/// Encode a `Param` as text-format bytes for use in a `Bind` message.
///
/// Returns `None` for `Param::Null` (wire protocol uses -1 length for NULL).
fn param_to_text_bytes(param: &Param) -> Option<Vec<u8>> {
    match param {
        Param::Null => None,
        Param::Bool(b) => Some(if *b { b"t".to_vec() } else { b"f".to_vec() }),
        Param::Int4(n) => Some(n.to_string().into_bytes()),
        Param::Int8(n) => Some(n.to_string().into_bytes()),
        Param::Float8(f) => Some(f.to_string().into_bytes()),
        Param::Text(s) => Some(s.as_bytes().to_vec()),
        Param::Bytes(b) => Some(b.clone()),
    }
}

// ── PreparedStatement ─────────────────────────────────────────────────────────

/// A prepared statement returned by `PgConnection::prepare`.
///
/// Holds the statement name, inferred parameter OIDs, and column metadata.
/// Re-use this across multiple `execute_prepared` calls for efficient execution.
#[derive(Debug, Clone)]
pub struct PreparedStatement {
    /// The server-side statement name (empty string = unnamed, one-shot).
    pub(crate) name: String,
    /// Parameter type OIDs as reported by the server (`ParameterDescription`).
    pub param_types: Vec<u32>,
    /// Result column metadata as reported by the server (`RowDescription`).
    pub columns: Vec<PgColumn>,
}

impl PreparedStatement {
    /// The number of parameters this statement expects.
    pub fn param_count(&self) -> usize {
        self.param_types.len()
    }

    /// The column metadata for this statement's result set.
    pub fn columns(&self) -> &[PgColumn] {
        &self.columns
    }

    /// Whether this statement returns rows (false for INSERT/UPDATE/DELETE).
    pub fn returns_rows(&self) -> bool {
        !self.columns.is_empty()
    }
}

// ── prepare ───────────────────────────────────────────────────────────────────

/// Send Parse + Describe + Sync and read the resulting messages.
///
/// On success returns a `PreparedStatement` with full type and column metadata.
///
/// # Protocol sequence (frontend → backend)
/// ```text
/// Parse(name, sql, []) →
/// Describe('S', name) →
/// Sync →
///
/// ← ParseComplete
/// ← ParameterDescription(oids)
/// ← RowDescription(columns) | NoData
/// ← ReadyForQuery
/// ```
pub async fn prepare(
    stream: &mut TcpStream,
    name: &str,
    sql: &str,
) -> Result<PreparedStatement> {
    // Build and send the three messages as a single write (pipeline)
    let parse_payload = encode_parse(name, sql, &[]);
    let describe_payload = encode_describe(b'S', name);
    let sync_payload = encode_sync();

    write_frontend_message(stream, MSG_PARSE, &parse_payload).await?;
    write_frontend_message(stream, MSG_DESCRIBE, &describe_payload).await?;
    write_frontend_message(stream, MSG_SYNC, &sync_payload).await?;

    // Read responses
    let mut param_types: Vec<u32> = Vec::new();
    let mut columns: Vec<PgColumn> = Vec::new();

    loop {
        let (msg_type, payload) = read_backend_message(stream).await?;
        match decode_backend(msg_type, &payload)? {
            BackendMessage::ParseComplete => {
                // Expected — parse succeeded
            }
            BackendMessage::ParameterDescription(oids) => {
                param_types = oids;
            }
            BackendMessage::RowDescription(col_descs) => {
                columns = col_descs
                    .into_iter()
                    .map(|d: ColumnDesc| PgColumn {
                        name: d.name,
                        type_oid: d.type_oid,
                    })
                    .collect();
            }
            BackendMessage::NoData => {
                // Statement returns no rows (e.g. INSERT/UPDATE/DELETE)
                // columns stays empty
            }
            BackendMessage::ReadyForQuery { status } => {
                if status == b'E' {
                    return Err(DbError::Protocol(
                        "server in error state after prepare".into(),
                    ));
                }
                break;
            }
            BackendMessage::ErrorResponse { code, message, detail } => {
                // Drain until ReadyForQuery before returning error
                drain_until_ready(stream).await?;
                return Err(DbError::ServerError { code, message, detail });
            }
            BackendMessage::ParameterStatus { .. } | BackendMessage::NoticeResponse => {}
            other => {
                return Err(DbError::Protocol(format!(
                    "unexpected message during prepare: {other:?}"
                )));
            }
        }
    }

    Ok(PreparedStatement {
        name: name.to_string(),
        param_types,
        columns,
    })
}

// ── execute_prepared ──────────────────────────────────────────────────────────

/// Send Bind + Execute + Sync and collect the result rows.
///
/// Uses text format for both parameter values and result columns.
///
/// # Protocol sequence (frontend → backend)
/// ```text
/// Bind(portal="", stmt=name, params) →
/// Execute(portal="", max_rows=0) →
/// Sync →
///
/// ← BindComplete
/// ← DataRow* (zero or more)
/// ← CommandComplete
/// ← ReadyForQuery
/// ```
pub async fn execute_prepared(
    stream: &mut TcpStream,
    stmt: &PreparedStatement,
    params: &[Param],
) -> Result<PgRowSet> {
    // Encode params as text-format byte slices (None = NULL)
    let encoded: Vec<Option<Vec<u8>>> = params.iter().map(param_to_text_bytes).collect();

    // Use unnamed portal ("") — destroyed after Execute
    let bind_payload = encode_bind("", &stmt.name, &encoded);
    let execute_payload = encode_execute("", 0); // 0 = no row limit
    let sync_payload = encode_sync();

    write_frontend_message(stream, MSG_BIND, &bind_payload).await?;
    write_frontend_message(stream, MSG_EXECUTE, &execute_payload).await?;
    write_frontend_message(stream, MSG_SYNC, &sync_payload).await?;

    // Use column metadata from the prepared statement
    let columns: Vec<PgColumn> = stmt.columns.clone();
    let mut rows: Vec<PgRow> = Vec::new();

    loop {
        let (msg_type, payload) = read_backend_message(stream).await?;
        match decode_backend(msg_type, &payload)? {
            BackendMessage::BindComplete => {
                // Expected
            }
            BackendMessage::DataRow(fields) => {
                rows.push(PgRow {
                    columns: columns.clone(),
                    fields,
                });
            }
            BackendMessage::CommandComplete { .. } => {}
            BackendMessage::ReadyForQuery { status } => {
                if status == b'E' {
                    return Err(DbError::Protocol(
                        "server in error state after execute_prepared".into(),
                    ));
                }
                break;
            }
            BackendMessage::ErrorResponse { code, message, detail } => {
                drain_until_ready(stream).await?;
                return Err(DbError::ServerError { code, message, detail });
            }
            BackendMessage::ParameterStatus { .. } | BackendMessage::NoticeResponse => {}
            other => {
                return Err(DbError::Protocol(format!(
                    "unexpected message during execute_prepared: {other:?}"
                )));
            }
        }
    }

    Ok(PgRowSet { columns, rows })
}

// ── close_statement ───────────────────────────────────────────────────────────

/// Send a `Close` message to release a named prepared statement on the server.
///
/// Only needed for named statements (not the unnamed `""`).
/// Followed by Sync to get CloseComplete + ReadyForQuery.
pub async fn close_statement(stream: &mut TcpStream, name: &str) -> Result<()> {
    let close_payload = encode_close(b'S', name);
    let sync_payload = encode_sync();
    write_frontend_message(stream, MSG_CLOSE, &close_payload).await?;
    write_frontend_message(stream, MSG_SYNC, &sync_payload).await?;

    loop {
        let (msg_type, payload) = read_backend_message(stream).await?;
        match decode_backend(msg_type, &payload)? {
            BackendMessage::ReadyForQuery { .. } => break,
            BackendMessage::ErrorResponse { code, message, detail } => {
                drain_until_ready(stream).await?;
                return Err(DbError::ServerError { code, message, detail });
            }
            _ => {} // CloseComplete, NoticeResponse, etc.
        }
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read and discard messages until `ReadyForQuery` (error recovery).
async fn drain_until_ready(stream: &mut TcpStream) -> Result<()> {
    loop {
        let (msg_type, payload) = read_backend_message(stream).await?;
        if let BackendMessage::ReadyForQuery { .. } = decode_backend(msg_type, &payload)? {
            return Ok(());
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_to_oid_mapping() {
        assert_eq!(param_to_oid(&Param::Null), 0);
        assert_eq!(param_to_oid(&Param::Bool(true)), 16);
        assert_eq!(param_to_oid(&Param::Int4(1)), 23);
        assert_eq!(param_to_oid(&Param::Int8(1)), 20);
        assert_eq!(param_to_oid(&Param::Float8(1.0)), 701);
        assert_eq!(param_to_oid(&Param::Text("x".into())), 25);
        assert_eq!(param_to_oid(&Param::Bytes(vec![])), 17);
    }

    #[test]
    fn param_to_text_bytes_values() {
        assert_eq!(param_to_text_bytes(&Param::Null), None);
        assert_eq!(
            param_to_text_bytes(&Param::Bool(true)),
            Some(b"t".to_vec())
        );
        assert_eq!(
            param_to_text_bytes(&Param::Bool(false)),
            Some(b"f".to_vec())
        );
        assert_eq!(
            param_to_text_bytes(&Param::Int4(42)),
            Some(b"42".to_vec())
        );
        assert_eq!(
            param_to_text_bytes(&Param::Int8(-1)),
            Some(b"-1".to_vec())
        );
        assert_eq!(
            param_to_text_bytes(&Param::Text("hello".into())),
            Some(b"hello".to_vec())
        );
    }

    #[test]
    fn prepared_statement_accessors() {
        let stmt = PreparedStatement {
            name: "my_stmt".into(),
            param_types: vec![23, 25],
            columns: vec![
                PgColumn {
                    name: "id".into(),
                    type_oid: 23,
                },
                PgColumn {
                    name: "name".into(),
                    type_oid: 25,
                },
            ],
        };
        assert_eq!(stmt.param_count(), 2);
        assert_eq!(stmt.columns().len(), 2);
        assert!(stmt.returns_rows());
    }

    #[test]
    fn prepared_statement_no_columns_does_not_return_rows() {
        let stmt = PreparedStatement {
            name: "ins".into(),
            param_types: vec![25],
            columns: vec![],
        };
        assert!(!stmt.returns_rows());
    }

    #[test]
    fn encode_bind_roundtrip_structure() {
        // Verify that encode_bind produces a parseable payload for a single text param
        let param_data = Some(b"hello".to_vec());
        let payload = encode_bind("", "stmt1", &[param_data]);

        // Portal = "\0", Stmt = "stmt1\0"
        assert!(payload.starts_with(b"\0stmt1\0"));
        let offset = 7; // "\0stmt1\0" = 7 bytes
        // format codes count = 0
        let fmt_count = i16::from_be_bytes([payload[offset], payload[offset + 1]]);
        assert_eq!(fmt_count, 0);
        // param count = 1
        let param_count = i16::from_be_bytes([payload[offset + 2], payload[offset + 3]]);
        assert_eq!(param_count, 1);
        // param length = 5
        let param_len = i32::from_be_bytes([
            payload[offset + 4],
            payload[offset + 5],
            payload[offset + 6],
            payload[offset + 7],
        ]);
        assert_eq!(param_len, 5);
        // param data = "hello"
        assert_eq!(&payload[offset + 8..offset + 13], b"hello");
    }
}
