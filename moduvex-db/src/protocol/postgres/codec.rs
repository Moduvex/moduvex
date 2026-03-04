//! Encode/decode PostgreSQL frontend and backend messages (simple query protocol).
//!
//! Frontend (client→server):
//!   - `StartupMessage`  — open connection, send user/database params
//!   - `Query`           — simple query (Q)
//!   - `PasswordMessage` — auth response (p)
//!   - `Terminate`       — close connection (X)
//!
//! Backend (server→client):
//!   - `AuthenticationOk`          (R, sub=0)
//!   - `AuthenticationMD5Password` (R, sub=5)
//!   - `ParameterStatus`           (S)
//!   - `ReadyForQuery`             (Z)
//!   - `RowDescription`            (T)
//!   - `DataRow`                   (D)
//!   - `CommandComplete`           (C)
//!   - `ErrorResponse`             (E)
//!   - `NoticeResponse`            (N) — silently ignored

use crate::error::{DbError, Result};
use crate::protocol::postgres::pg_types::PgType;
use crate::protocol::postgres::wire::{read_cstring, write_cstring};

// ── Frontend message type bytes ───────────────────────────────────────────────

pub const MSG_QUERY:    u8 = b'Q';
pub const MSG_PASSWORD: u8 = b'p';
pub const MSG_TERMINATE:u8 = b'X';

// ── Backend message type bytes ────────────────────────────────────────────────

pub const MSG_AUTH:             u8 = b'R';
pub const MSG_PARAM_STATUS:     u8 = b'S';
pub const MSG_READY_FOR_QUERY:  u8 = b'Z';
pub const MSG_ROW_DESC:         u8 = b'T';
pub const MSG_DATA_ROW:         u8 = b'D';
pub const MSG_COMMAND_COMPLETE: u8 = b'C';
pub const MSG_ERROR_RESPONSE:   u8 = b'E';
pub const MSG_NOTICE_RESPONSE:  u8 = b'N';

// ── Frontend encoders ─────────────────────────────────────────────────────────

/// Build the startup message payload.
///
/// Format: `[protocol_version: i32 BE][key\0value\0 ...][0]`
/// Protocol version 3.0 = `0x00030000`.
pub fn encode_startup(user: &str, database: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    // Protocol version 3.0
    buf.extend_from_slice(&0x0003_0000_u32.to_be_bytes());
    write_cstring(&mut buf, "user");
    write_cstring(&mut buf, user);
    write_cstring(&mut buf, "database");
    write_cstring(&mut buf, database);
    // Terminator
    buf.push(0);
    buf
}

/// Build a `Query` message payload (just the null-terminated SQL string).
pub fn encode_query(sql: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(sql.len() + 1);
    write_cstring(&mut buf, sql);
    buf
}

/// Build a `PasswordMessage` payload.
pub fn encode_password(password: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(password.len() + 1);
    write_cstring(&mut buf, password);
    buf
}

// ── Backend decoders ──────────────────────────────────────────────────────────

/// Decoded backend message variants.
#[derive(Debug)]
pub enum BackendMessage {
    /// Authentication succeeded.
    AuthOk,
    /// Server requests MD5 password with the given 4-byte salt.
    AuthMd5 { salt: [u8; 4] },
    /// Server parameter (e.g. `server_version`, `TimeZone`).
    ParameterStatus { name: String, value: String },
    /// Server is ready to accept a new command. `status` = 'I' (idle), 'T' (in tx), 'E' (error).
    ReadyForQuery { status: u8 },
    /// Column metadata for an upcoming result set.
    RowDescription(Vec<ColumnDesc>),
    /// One row of data — each element is `None` for SQL NULL, or raw text bytes.
    DataRow(Vec<Option<Vec<u8>>>),
    /// Query completed; tag is e.g. `"SELECT 3"` or `"INSERT 0 1"`.
    CommandComplete { tag: String },
    /// Server error.
    ErrorResponse {
        code: String,
        message: String,
        detail: Option<String>,
    },
    /// Ignored notice.
    NoticeResponse,
}

/// Column metadata from a `RowDescription` message.
#[derive(Debug, Clone)]
pub struct ColumnDesc {
    pub name: String,
    pub type_oid: u32,
    pub pg_type: PgType,
}

/// Decode a backend message from `(type_byte, payload)`.
pub fn decode_backend(msg_type: u8, payload: &[u8]) -> Result<BackendMessage> {
    match msg_type {
        MSG_AUTH => decode_auth(payload),
        MSG_PARAM_STATUS => decode_param_status(payload),
        MSG_READY_FOR_QUERY => decode_ready_for_query(payload),
        MSG_ROW_DESC => decode_row_description(payload),
        MSG_DATA_ROW => decode_data_row(payload),
        MSG_COMMAND_COMPLETE => decode_command_complete(payload),
        MSG_ERROR_RESPONSE => decode_error_response(payload),
        MSG_NOTICE_RESPONSE => Ok(BackendMessage::NoticeResponse),
        other => Err(DbError::Protocol(format!(
            "unexpected backend message type: 0x{other:02X}"
        ))),
    }
}

// ── Internal decoders ─────────────────────────────────────────────────────────

fn decode_auth(payload: &[u8]) -> Result<BackendMessage> {
    if payload.len() < 4 {
        return Err(DbError::Protocol("auth message too short".into()));
    }
    let sub = i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    match sub {
        0 => Ok(BackendMessage::AuthOk),
        5 => {
            if payload.len() < 8 {
                return Err(DbError::Protocol("MD5 auth message missing salt".into()));
            }
            let salt = [payload[4], payload[5], payload[6], payload[7]];
            Ok(BackendMessage::AuthMd5 { salt })
        }
        other => Err(DbError::UnsupportedAuth(format!("auth type {other}"))),
    }
}

fn decode_param_status(payload: &[u8]) -> Result<BackendMessage> {
    let (name, off) = read_cstring(payload, 0)?;
    let (value, _) = read_cstring(payload, off)?;
    Ok(BackendMessage::ParameterStatus { name, value })
}

fn decode_ready_for_query(payload: &[u8]) -> Result<BackendMessage> {
    if payload.is_empty() {
        return Err(DbError::Protocol("ReadyForQuery payload empty".into()));
    }
    Ok(BackendMessage::ReadyForQuery { status: payload[0] })
}

fn decode_row_description(payload: &[u8]) -> Result<BackendMessage> {
    if payload.len() < 2 {
        return Err(DbError::Protocol("RowDescription too short".into()));
    }
    let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut columns = Vec::with_capacity(count);
    let mut pos = 2;
    for _ in 0..count {
        let (name, next) = read_cstring(payload, pos)?;
        pos = next;
        // table_oid(4) + attr_num(2) + type_oid(4) + type_size(2) + type_modifier(4) + format(2)
        if pos + 18 > payload.len() {
            return Err(DbError::Protocol("RowDescription column entry truncated".into()));
        }
        let type_oid = u32::from_be_bytes([payload[pos+6], payload[pos+7], payload[pos+8], payload[pos+9]]);
        pos += 18;
        columns.push(ColumnDesc {
            name,
            type_oid,
            pg_type: PgType::from_oid(type_oid),
        });
    }
    Ok(BackendMessage::RowDescription(columns))
}

fn decode_data_row(payload: &[u8]) -> Result<BackendMessage> {
    if payload.len() < 2 {
        return Err(DbError::Protocol("DataRow too short".into()));
    }
    let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut fields = Vec::with_capacity(count);
    let mut pos = 2;
    for _ in 0..count {
        if pos + 4 > payload.len() {
            return Err(DbError::Protocol("DataRow field length truncated".into()));
        }
        let len = i32::from_be_bytes([payload[pos], payload[pos+1], payload[pos+2], payload[pos+3]]);
        pos += 4;
        if len == -1 {
            fields.push(None); // SQL NULL
        } else {
            let len = len as usize;
            if pos + len > payload.len() {
                return Err(DbError::Protocol("DataRow field data truncated".into()));
            }
            fields.push(Some(payload[pos..pos+len].to_vec()));
            pos += len;
        }
    }
    Ok(BackendMessage::DataRow(fields))
}

fn decode_command_complete(payload: &[u8]) -> Result<BackendMessage> {
    let (tag, _) = read_cstring(payload, 0)?;
    Ok(BackendMessage::CommandComplete { tag })
}

fn decode_error_response(payload: &[u8]) -> Result<BackendMessage> {
    let mut code = String::new();
    let mut message = String::new();
    let mut detail = None;
    let mut pos = 0;
    while pos < payload.len() {
        let field_type = payload[pos];
        pos += 1;
        if field_type == 0 { break; }
        let (value, next) = read_cstring(payload, pos)?;
        pos = next;
        match field_type {
            b'C' => code = value,
            b'M' => message = value,
            b'D' => detail = Some(value),
            _ => {}
        }
    }
    Ok(BackendMessage::ErrorResponse { code, message, detail })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_payload_contains_user_and_database() {
        let payload = encode_startup("alice", "mydb");
        // Protocol version bytes
        assert_eq!(&payload[0..4], &0x0003_0000_u32.to_be_bytes());
        let s = String::from_utf8_lossy(&payload);
        assert!(s.contains("user"));
        assert!(s.contains("alice"));
        assert!(s.contains("database"));
        assert!(s.contains("mydb"));
        // Terminating null
        assert_eq!(*payload.last().unwrap(), 0);
    }

    #[test]
    fn query_payload_is_null_terminated() {
        let payload = encode_query("SELECT 1");
        assert_eq!(*payload.last().unwrap(), 0);
        assert!(payload.starts_with(b"SELECT 1"));
    }

    #[test]
    fn decode_auth_ok() {
        let payload = 0i32.to_be_bytes().to_vec();
        let msg = decode_backend(MSG_AUTH, &payload).unwrap();
        assert!(matches!(msg, BackendMessage::AuthOk));
    }

    #[test]
    fn decode_auth_md5_extracts_salt() {
        let mut payload = 5i32.to_be_bytes().to_vec();
        payload.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let msg = decode_backend(MSG_AUTH, &payload).unwrap();
        assert!(matches!(msg, BackendMessage::AuthMd5 { salt: [0xAA, 0xBB, 0xCC, 0xDD] }));
    }

    #[test]
    fn decode_ready_for_query() {
        let payload = vec![b'I'];
        let msg = decode_backend(MSG_READY_FOR_QUERY, &payload).unwrap();
        assert!(matches!(msg, BackendMessage::ReadyForQuery { status: b'I' }));
    }

    #[test]
    fn decode_command_complete() {
        let mut payload = b"SELECT 3".to_vec();
        payload.push(0);
        let msg = decode_backend(MSG_COMMAND_COMPLETE, &payload).unwrap();
        assert!(matches!(msg, BackendMessage::CommandComplete { ref tag } if tag == "SELECT 3"));
    }

    #[test]
    fn decode_param_status() {
        let mut payload = Vec::new();
        write_cstring(&mut payload, "server_version");
        write_cstring(&mut payload, "14.0");
        let msg = decode_backend(MSG_PARAM_STATUS, &payload).unwrap();
        assert!(matches!(
            msg,
            BackendMessage::ParameterStatus { ref name, ref value }
                if name == "server_version" && value == "14.0"
        ));
    }

    #[test]
    fn decode_error_response_extracts_fields() {
        let mut payload = Vec::new();
        payload.push(b'C'); write_cstring(&mut payload, "23505");
        payload.push(b'M'); write_cstring(&mut payload, "duplicate key");
        payload.push(b'D'); write_cstring(&mut payload, "Key (id)=(1) already exists.");
        payload.push(0);
        let msg = decode_backend(MSG_ERROR_RESPONSE, &payload).unwrap();
        match msg {
            BackendMessage::ErrorResponse { code, message, detail } => {
                assert_eq!(code, "23505");
                assert_eq!(message, "duplicate key");
                assert_eq!(detail.unwrap(), "Key (id)=(1) already exists.");
            }
            _ => panic!("expected ErrorResponse"),
        }
    }

    #[test]
    fn decode_data_row_with_null() {
        // 2 fields: "hello" then NULL
        let mut payload = Vec::new();
        payload.extend_from_slice(&2u16.to_be_bytes());
        // Field 1: len=5, "hello"
        payload.extend_from_slice(&5i32.to_be_bytes());
        payload.extend_from_slice(b"hello");
        // Field 2: NULL (-1)
        payload.extend_from_slice(&(-1i32).to_be_bytes());
        let msg = decode_backend(MSG_DATA_ROW, &payload).unwrap();
        match msg {
            BackendMessage::DataRow(fields) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0], Some(b"hello".to_vec()));
                assert_eq!(fields[1], None);
            }
            _ => panic!("expected DataRow"),
        }
    }
}
