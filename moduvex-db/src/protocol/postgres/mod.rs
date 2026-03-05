//! PostgreSQL protocol entry — `PgConnection` wraps a `TcpStream` and provides
//! connect/query/terminate over the simple query protocol.
//!
//! Supports both MD5 (legacy) and SCRAM-SHA-256 (PostgreSQL 14+ default) auth.

pub mod auth;
pub mod codec;
pub mod pg_types;
pub mod prepared;
pub mod wire;

use std::collections::HashMap;
use std::time::Instant;

use moduvex_runtime::net::TcpStream;

use crate::error::{DbError, Result};
use crate::protocol::postgres::auth::md5_password;
use crate::protocol::postgres::auth::scram_sha256::{
    ScramClient,
    AUTH_SASL, AUTH_SASL_CONTINUE, AUTH_SASL_FINAL,
    decode_sasl_mechanisms, decode_sasl_continue, decode_sasl_final,
    encode_sasl_initial_response, encode_sasl_response,
};
use crate::protocol::postgres::codec::{
    decode_backend, encode_password, encode_query, encode_startup, BackendMessage, ColumnDesc,
    MSG_AUTH, MSG_PASSWORD, MSG_QUERY, MSG_TERMINATE,
};
use crate::protocol::postgres::prepared::{
    PreparedStatement,
    close_statement, execute_prepared as exec_prepared, prepare as pg_prepare,
};
use crate::protocol::postgres::wire::{
    read_backend_message, write_frontend_message, write_startup_message,
};
use crate::query::param::Param;

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
    /// When this connection was first opened (set once, never reset).
    pub(crate) created_at: Instant,
}

impl PgConnection {
    /// Connect and authenticate to a PostgreSQL server.
    ///
    /// Supports MD5 (legacy) and SCRAM-SHA-256 (PostgreSQL 14+ default).
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

            // Intercept SASL auth messages before decode_backend, which does not
            // handle auth sub-types 10/11/12 (returns UnsupportedAuth for them).
            if msg_type == MSG_AUTH && is_sasl_auth(&payload) {
                perform_scram_auth(&mut stream, user, password, &payload).await?;
                // After SCRAM completes we expect AuthOk followed by params + ReadyForQuery
                continue;
            }

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
                    return Ok(PgConnection { stream, params, created_at: Instant::now() });
                }
                BackendMessage::ErrorResponse { code, message, detail } => {
                    return Err(DbError::ServerError { code, message, detail });
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
                BackendMessage::ErrorResponse { code, message, detail } => {
                    self.drain_until_ready().await?;
                    return Err(DbError::ServerError { code, message, detail });
                }
                BackendMessage::ParameterStatus { .. } | BackendMessage::NoticeResponse => {}
                BackendMessage::AuthOk | BackendMessage::AuthMd5 { .. } => {
                    return Err(DbError::Protocol(
                        "unexpected auth message during query".into(),
                    ));
                }
                // Extended query protocol messages are unexpected during simple query
                BackendMessage::ParseComplete
                | BackendMessage::BindComplete
                | BackendMessage::NoData
                | BackendMessage::ParameterDescription(_) => {
                    return Err(DbError::Protocol(
                        "unexpected extended protocol message during simple query".into(),
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
                BackendMessage::ErrorResponse { code, message, detail } => {
                    self.drain_until_ready().await?;
                    return Err(DbError::ServerError { code, message, detail });
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

    // ── Extended query protocol ───────────────────────────────────────────────

    /// Prepare a SQL statement using the extended query protocol.
    ///
    /// Sends Parse + Describe + Sync to the server and returns a
    /// `PreparedStatement` with parameter type OIDs and column metadata.
    ///
    /// Use `name = ""` for an unnamed (single-use) statement; use a unique
    /// name if you plan to reuse it across multiple `execute_prepared` calls.
    ///
    /// # Example
    /// ```rust,ignore
    /// let stmt = conn.prepare("SELECT id FROM users WHERE name = $1").await?;
    /// let rows = conn.execute_prepared(&stmt, &[Param::Text("Alice".into())]).await?;
    /// ```
    pub async fn prepare(&mut self, sql: &str) -> Result<PreparedStatement> {
        pg_prepare(&mut self.stream, "", sql).await
    }

    /// Prepare a named statement (survives until `close_prepared` or disconnect).
    ///
    /// Named statements allow the server to cache the query plan for repeated use.
    pub async fn prepare_named(&mut self, name: &str, sql: &str) -> Result<PreparedStatement> {
        pg_prepare(&mut self.stream, name, sql).await
    }

    /// Execute a previously prepared statement with bound parameters.
    ///
    /// Returns all result rows. Use text format for parameters and results.
    pub async fn execute_prepared(
        &mut self,
        stmt: &PreparedStatement,
        params: &[Param],
    ) -> Result<PgRowSet> {
        exec_prepared(&mut self.stream, stmt, params).await
    }

    /// Close a named prepared statement to release server-side resources.
    ///
    /// Only needed for named statements. Unnamed (`""`) statements are
    /// automatically released on next Parse.
    pub async fn close_prepared(&mut self, name: &str) -> Result<()> {
        close_statement(&mut self.stream, name).await
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

// ── SCRAM-SHA-256 handshake ───────────────────────────────────────────────────

/// Returns true if the auth payload sub-type is 10 (SASL), 11 (SASLContinue), or 12 (SASLFinal).
fn is_sasl_auth(payload: &[u8]) -> bool {
    if payload.len() < 4 {
        return false;
    }
    let sub = i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    matches!(sub, AUTH_SASL | AUTH_SASL_CONTINUE | AUTH_SASL_FINAL)
}

/// Perform the full SCRAM-SHA-256 exchange starting from the AuthenticationSASL message.
///
/// On entry, `initial_payload` is the raw payload of the first auth message (sub-type 10).
/// After this function returns `Ok(())`, the caller should continue the main loop to
/// receive `AuthOk`, `ParameterStatus`, and `ReadyForQuery`.
async fn perform_scram_auth(
    stream: &mut TcpStream,
    user: &str,
    password: &str,
    initial_payload: &[u8],
) -> Result<()> {
    // Step 1: decode mechanism list from AuthenticationSASL (sub=10)
    let mechanisms = decode_sasl_mechanisms(initial_payload)?;

    // Prefer SCRAM-SHA-256 (RFC 7677); reject if not offered
    let mechanism = mechanisms
        .iter()
        .find(|m| m.as_str() == "SCRAM-SHA-256")
        .ok_or_else(|| {
            DbError::UnsupportedAuth(format!(
                "server offered SASL mechanisms: {mechanisms:?}; SCRAM-SHA-256 not among them"
            ))
        })?;

    // Step 2: send SASLInitialResponse with client-first-message
    let scram = ScramClient::new(user, password);
    let client_first = scram.client_first_message();
    let sasl_init = encode_sasl_initial_response(mechanism, &client_first);
    write_frontend_message(stream, MSG_PASSWORD, &sasl_init).await?;

    // Step 3: receive AuthenticationSASLContinue (sub=11)
    let (msg_type, payload) = read_backend_message(stream).await?;
    if msg_type != MSG_AUTH {
        return Err(DbError::Protocol(format!(
            "expected auth message after SASLInitialResponse, got 0x{msg_type:02X}"
        )));
    }
    let sub = if payload.len() >= 4 {
        i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]])
    } else {
        return Err(DbError::Protocol("auth payload too short for sub-type".into()));
    };
    if sub != AUTH_SASL_CONTINUE {
        return Err(DbError::Protocol(format!(
            "expected AuthSASLContinue (sub=11), got sub={sub}"
        )));
    }
    let server_first = decode_sasl_continue(&payload)?;

    // Step 4: compute client-final-message and expected server signature
    let (client_final, expected_server_sig) = scram.process_server_first(&server_first)?;

    // Step 5: send SASLResponse with client-final-message
    let sasl_resp = encode_sasl_response(&client_final);
    write_frontend_message(stream, MSG_PASSWORD, &sasl_resp).await?;

    // Step 6: receive AuthenticationSASLFinal (sub=12) — server signature for mutual auth
    let (msg_type, payload) = read_backend_message(stream).await?;
    if msg_type != MSG_AUTH {
        return Err(DbError::Protocol(format!(
            "expected auth message after SASLResponse, got 0x{msg_type:02X}"
        )));
    }
    let sub = if payload.len() >= 4 {
        i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]])
    } else {
        return Err(DbError::Protocol("auth payload too short for sub-type".into()));
    };
    if sub != AUTH_SASL_FINAL {
        return Err(DbError::Protocol(format!(
            "expected AuthSASLFinal (sub=12), got sub={sub}"
        )));
    }
    let server_final = decode_sasl_final(&payload)?;

    // Verify server signature (mutual authentication)
    scram.verify_server_final(&server_final, &expected_server_sig)?;

    // Step 7: receive AuthenticationOk (sub=0)
    let (msg_type, payload) = read_backend_message(stream).await?;
    match decode_backend(msg_type, &payload)? {
        BackendMessage::AuthOk => Ok(()),
        BackendMessage::ErrorResponse { code, message, detail } => {
            Err(DbError::ServerError { code, message, detail })
        }
        other => Err(DbError::Protocol(format!(
            "expected AuthOk after SCRAM, got: {other:?}"
        ))),
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

    #[test]
    fn is_sasl_auth_detects_sub_types() {
        // sub=10 (SASL)
        let p: Vec<u8> = 10i32.to_be_bytes().to_vec();
        assert!(is_sasl_auth(&p));
        // sub=11 (SASLContinue)
        let p: Vec<u8> = 11i32.to_be_bytes().to_vec();
        assert!(is_sasl_auth(&p));
        // sub=12 (SASLFinal)
        let p: Vec<u8> = 12i32.to_be_bytes().to_vec();
        assert!(is_sasl_auth(&p));
        // sub=0 (AuthOk) — not SASL
        let p: Vec<u8> = 0i32.to_be_bytes().to_vec();
        assert!(!is_sasl_auth(&p));
        // sub=5 (MD5) — not SASL
        let p: Vec<u8> = 5i32.to_be_bytes().to_vec();
        assert!(!is_sasl_auth(&p));
        // too short
        assert!(!is_sasl_auth(&[0u8; 3]));
    }
}
