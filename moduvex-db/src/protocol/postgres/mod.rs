//! PostgreSQL protocol entry — `PgConnection` wraps a `TcpStream` and provides
//! connect/query/terminate over the simple query protocol.

pub mod auth;
pub mod codec;
pub mod pg_types;
pub mod wire;

use std::collections::HashMap;

use moduvex_runtime::net::TcpStream;

use crate::error::{DbError, Result};
use crate::protocol::postgres::auth::md5_password;
use crate::protocol::postgres::codec::{
    decode_backend, encode_password, encode_query, encode_startup, BackendMessage, ColumnDesc,
    MSG_PASSWORD, MSG_QUERY, MSG_TERMINATE,
};
use crate::protocol::postgres::wire::{
    read_backend_message, write_frontend_message, write_startup_message,
};

// ── Row/Column/RowSet — defined here to avoid circular dep with query module ──

/// Metadata for a single result column (mirrors query::Column, used internally).
#[derive(Debug, Clone)]
pub struct PgColumn {
    pub name: String,
    pub type_oid: u32,
}

/// Raw row data from PostgreSQL — fields are text-format bytes or None (NULL).
#[derive(Debug, Clone)]
pub struct PgRow {
    pub columns: Vec<PgColumn>,
    pub fields: Vec<Option<Vec<u8>>>,
}

/// Complete result set: column metadata + all rows.
#[derive(Debug)]
pub struct PgRowSet {
    pub columns: Vec<PgColumn>,
    pub rows: Vec<PgRow>,
}

impl PgRowSet {
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &PgRow> {
        self.rows.iter()
    }
}

// ── PgConnection ──────────────────────────────────────────────────────────────

/// An authenticated PostgreSQL connection using the simple query protocol.
pub struct PgConnection {
    stream: TcpStream,
    /// Server parameters received during startup (e.g. `server_version`).
    params: HashMap<String, String>,
}

impl PgConnection {
    /// Connect and authenticate to a PostgreSQL server.
    ///
    /// # Arguments
    /// * `addr`     — host:port as `&str` (e.g. `"127.0.0.1:5432"`)
    /// * `user`     — PostgreSQL username
    /// * `password` — plaintext password
    /// * `database` — database name
    pub async fn connect(addr: &str, user: &str, password: &str, database: &str) -> Result<Self> {
        let sock_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| DbError::Other(format!("invalid address '{addr}': {e}")))?;

        let mut stream = TcpStream::connect(sock_addr).await?;

        // Send startup message (no type byte prefix)
        let startup_payload = encode_startup(user, database);
        write_startup_message(&mut stream, &startup_payload).await?;

        // Drive the auth handshake
        let mut params = HashMap::new();
        loop {
            let (msg_type, payload) = read_backend_message(&mut stream).await?;
            match decode_backend(msg_type, &payload)? {
                BackendMessage::AuthOk => {
                    // Auth succeeded; wait for ReadyForQuery
                }
                BackendMessage::AuthMd5 { salt } => {
                    let response = md5_password(user, password, &salt);
                    let pw_payload = encode_password(&response);
                    write_frontend_message(&mut stream, MSG_PASSWORD, &pw_payload).await?;
                }
                BackendMessage::ParameterStatus { name, value } => {
                    params.insert(name, value);
                }
                BackendMessage::ReadyForQuery { .. } => {
                    return Ok(PgConnection { stream, params });
                }
                BackendMessage::ErrorResponse {
                    code,
                    message,
                    detail,
                } => {
                    return Err(DbError::ServerError {
                        code,
                        message,
                        detail,
                    });
                }
                BackendMessage::NoticeResponse => {}
                other => {
                    return Err(DbError::Protocol(format!(
                        "unexpected message during startup: {other:?}"
                    )));
                }
            }
        }
    }

    /// Execute a simple SQL query and return all rows.
    pub async fn query(&mut self, sql: &str) -> Result<PgRowSet> {
        let payload = encode_query(sql);
        write_frontend_message(&mut self.stream, MSG_QUERY, &payload).await?;

        let mut columns: Vec<PgColumn> = Vec::new();
        let mut rows: Vec<PgRow> = Vec::new();

        loop {
            let (msg_type, payload) = read_backend_message(&mut self.stream).await?;
            match decode_backend(msg_type, &payload)? {
                BackendMessage::RowDescription(col_descs) => {
                    columns = col_descs
                        .into_iter()
                        .map(|d: ColumnDesc| PgColumn {
                            name: d.name,
                            type_oid: d.type_oid,
                        })
                        .collect();
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
                            "server in error state after query".into(),
                        ));
                    }
                    break;
                }
                BackendMessage::ErrorResponse {
                    code,
                    message,
                    detail,
                } => {
                    self.drain_until_ready().await?;
                    return Err(DbError::ServerError {
                        code,
                        message,
                        detail,
                    });
                }
                BackendMessage::ParameterStatus { .. } | BackendMessage::NoticeResponse => {}
                BackendMessage::AuthOk | BackendMessage::AuthMd5 { .. } => {
                    return Err(DbError::Protocol(
                        "unexpected auth message during query".into(),
                    ));
                }
            }
        }

        Ok(PgRowSet { columns, rows })
    }

    /// Execute a simple SQL statement (INSERT/UPDATE/DELETE); return rows affected.
    pub async fn execute(&mut self, sql: &str) -> Result<u64> {
        let payload = encode_query(sql);
        write_frontend_message(&mut self.stream, MSG_QUERY, &payload).await?;

        let mut affected = 0u64;
        loop {
            let (msg_type, payload) = read_backend_message(&mut self.stream).await?;
            match decode_backend(msg_type, &payload)? {
                BackendMessage::CommandComplete { tag } => {
                    affected = parse_affected_rows(&tag);
                }
                BackendMessage::ReadyForQuery { .. } => break,
                BackendMessage::ErrorResponse {
                    code,
                    message,
                    detail,
                } => {
                    self.drain_until_ready().await?;
                    return Err(DbError::ServerError {
                        code,
                        message,
                        detail,
                    });
                }
                BackendMessage::RowDescription(_) | BackendMessage::DataRow(_) => {}
                BackendMessage::ParameterStatus { .. } | BackendMessage::NoticeResponse => {}
                other => {
                    return Err(DbError::Protocol(format!(
                        "unexpected message during execute: {other:?}"
                    )));
                }
            }
        }
        Ok(affected)
    }

    /// Send the `Terminate` message and close the connection gracefully.
    pub async fn terminate(mut self) -> Result<()> {
        write_frontend_message(&mut self.stream, MSG_TERMINATE, &[]).await
    }

    /// Check whether the connection is alive by sending a simple ping query.
    pub async fn ping(&mut self) -> Result<()> {
        self.execute("SELECT 1").await?;
        Ok(())
    }

    /// Return a server parameter value (from startup exchange).
    pub fn server_param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(|s| s.as_str())
    }

    /// Read and discard messages until `ReadyForQuery` is received.
    async fn drain_until_ready(&mut self) -> Result<()> {
        loop {
            let (msg_type, payload) = read_backend_message(&mut self.stream).await?;
            if let BackendMessage::ReadyForQuery { .. } = decode_backend(msg_type, &payload)? {
                return Ok(());
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse the rows-affected count from a `CommandComplete` tag.
///
/// Tags: `"INSERT 0 N"`, `"UPDATE N"`, `"DELETE N"`, `"SELECT N"`, etc.
fn parse_affected_rows(tag: &str) -> u64 {
    tag.split_whitespace()
        .last()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_affected_insert() {
        assert_eq!(parse_affected_rows("INSERT 0 3"), 3);
    }

    #[test]
    fn parse_affected_update() {
        assert_eq!(parse_affected_rows("UPDATE 5"), 5);
    }

    #[test]
    fn parse_affected_select() {
        assert_eq!(parse_affected_rows("SELECT 10"), 10);
    }

    #[test]
    fn parse_affected_unknown_tag() {
        assert_eq!(parse_affected_rows("CREATE TABLE"), 0);
    }
}
