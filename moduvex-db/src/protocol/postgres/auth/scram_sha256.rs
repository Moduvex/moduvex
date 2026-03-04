//! SCRAM-SHA-256 authentication (RFC 5802 / RFC 7677) for PostgreSQL 14+.
//!
//! Auth flow:
//!   1. Server sends `AuthenticationSASL` (auth type 10) with mechanism list
//!   2. Client sends `SASLInitialResponse` with client-first-message
//!   3. Server sends `AuthenticationSASLContinue` (auth type 11) with server-first-message
//!   4. Client computes proof and sends `SASLResponse`
//!   5. Server sends `AuthenticationSASLFinal` (auth type 12) with server signature
//!   6. Client verifies server signature (mutual auth)
//!
//! Crypto: sha2 + hmac crates (RustCrypto, same ecosystem as md-5 already in use).

use base64ct::{Base64, Encoding};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::error::{DbError, Result};

type HmacSha256 = Hmac<Sha256>;

// ── Auth sub-type constants ───────────────────────────────────────────────────

/// PostgreSQL wire protocol SASL auth sub-types.
pub const AUTH_SASL: i32 = 10;
pub const AUTH_SASL_CONTINUE: i32 = 11;
pub const AUTH_SASL_FINAL: i32 = 12;

// ── Crypto helpers ────────────────────────────────────────────────────────────

/// Compute HMAC-SHA-256(key, data), returning a 32-byte array.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Compute SHA-256(data), returning a 32-byte array.
fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Compute PBKDF2-HMAC-SHA-256(password, salt, iterations) → 32 bytes.
///
/// Per RFC 2898 §5.2, single output block (dkLen = 32 = PRF output length).
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    // U1 = HMAC(password, salt || INT(1))
    let mut salt_block = Vec::with_capacity(salt.len() + 4);
    salt_block.extend_from_slice(salt);
    salt_block.extend_from_slice(&1u32.to_be_bytes()); // block index = 1
    let mut u = hmac_sha256(password, &salt_block);
    let mut result = u;

    // Ui = HMAC(password, U_{i-1}); T = XOR of all Ui
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (r, &ui) in result.iter_mut().zip(u.iter()) {
            *r ^= ui;
        }
    }
    result
}

// ── Nonce generation ─────────────────────────────────────────────────────────

/// Generate a cryptographically random 24-character Base64 nonce.
///
/// Reads 18 bytes from `/dev/urandom` on Unix; falls back to time-seeded
/// mixing on other platforms or if the device is unavailable.
pub fn generate_nonce() -> String {
    let bytes = read_random_bytes();
    Base64::encode_string(&bytes)
}

fn read_random_bytes() -> [u8; 18] {
    let mut buf = [0u8; 18];

    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            if f.read_exact(&mut buf).is_ok() {
                return buf;
            }
        }
    }

    // Fallback: time-seeded mixing (non-Unix or /dev/urandom unavailable)
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = t.as_secs();
    let nanos = t.subsec_nanos() as u64;
    for (i, b) in buf.iter_mut().enumerate() {
        *b = secs
            .wrapping_add(i as u64)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(nanos)
            .to_le_bytes()[0];
    }
    buf
}

// ── SCRAM client ─────────────────────────────────────────────────────────────

/// SCRAM-SHA-256 client state — holds values needed across the two-message exchange.
pub struct ScramClient {
    pub username: String,
    pub password: String,
    pub client_nonce: String,
}

impl ScramClient {
    /// Create a new client with a freshly generated random nonce.
    pub fn new(username: &str, password: &str) -> Self {
        ScramClient {
            username: username.to_string(),
            password: password.to_string(),
            client_nonce: generate_nonce(),
        }
    }

    /// Build `client-first-message-bare` = `n=user,r=client-nonce`.
    pub fn client_first_bare(&self) -> String {
        format!("n={},r={}", self.username, self.client_nonce)
    }

    /// Build `client-first-message` = `n,,` + client-first-bare (no channel binding).
    pub fn client_first_message(&self) -> String {
        format!("n,,{}", self.client_first_bare())
    }

    /// Parse the server-first-message and compute the client-final-message.
    ///
    /// Returns `(client_final_message, expected_server_signature)`.
    pub fn process_server_first(&self, server_first: &str) -> Result<(String, Vec<u8>)> {
        // Parse: r=<combined_nonce>,s=<salt_b64>,i=<iterations>
        let combined_nonce = extract_attr(server_first, 'r').ok_or_else(|| {
            DbError::Protocol("SCRAM: missing 'r' in server-first".into())
        })?;
        let salt_b64 = extract_attr(server_first, 's').ok_or_else(|| {
            DbError::Protocol("SCRAM: missing 's' in server-first".into())
        })?;
        let iter_str = extract_attr(server_first, 'i').ok_or_else(|| {
            DbError::Protocol("SCRAM: missing 'i' in server-first".into())
        })?;

        // Validate: combined nonce must start with our client nonce
        if !combined_nonce.starts_with(&self.client_nonce) {
            return Err(DbError::AuthFailed(
                "SCRAM: server nonce does not contain client nonce".into(),
            ));
        }

        let salt = Base64::decode_vec(salt_b64)
            .map_err(|_| DbError::Protocol("SCRAM: invalid base64 salt".into()))?;
        let iterations: u32 = iter_str
            .parse()
            .map_err(|_| DbError::Protocol("SCRAM: invalid iteration count".into()))?;
        if iterations == 0 {
            return Err(DbError::Protocol("SCRAM: iteration count must be > 0".into()));
        }

        // Key derivation
        let salted_password = pbkdf2_hmac_sha256(self.password.as_bytes(), &salt, iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = sha256(&client_key);
        let server_key = hmac_sha256(&salted_password, b"Server Key");

        // channel-binding value = base64("n,,")  — gs2-header, no channel binding
        let channel_binding = Base64::encode_string(b"n,,");

        // client-final-message-without-proof
        let client_final_no_proof = format!("c={channel_binding},r={combined_nonce}");

        // AuthMessage = client-first-bare + "," + server-first + "," + client-final-without-proof
        let auth_message = format!(
            "{},{},{}",
            self.client_first_bare(),
            server_first,
            client_final_no_proof
        );

        // ClientProof = ClientKey XOR HMAC(StoredKey, AuthMessage)
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let mut client_proof = client_key;
        for (p, &s) in client_proof.iter_mut().zip(client_signature.iter()) {
            *p ^= s;
        }

        // ServerSignature = HMAC(ServerKey, AuthMessage) — for mutual auth verification
        let server_signature = hmac_sha256(&server_key, auth_message.as_bytes());

        let client_final = format!(
            "{},p={}",
            client_final_no_proof,
            Base64::encode_string(&client_proof)
        );

        Ok((client_final, server_signature.to_vec()))
    }

    /// Verify the server's signature in the server-final-message (mutual authentication).
    pub fn verify_server_final(&self, server_final: &str, expected: &[u8]) -> Result<()> {
        let v = extract_attr(server_final, 'v').ok_or_else(|| {
            DbError::Protocol("SCRAM: missing 'v' in server-final".into())
        })?;
        let server_sig = Base64::decode_vec(v)
            .map_err(|_| DbError::Protocol("SCRAM: invalid base64 server signature".into()))?;
        if server_sig != expected {
            return Err(DbError::AuthFailed(
                "SCRAM: server signature verification failed (mutual auth)".into(),
            ));
        }
        Ok(())
    }
}

// ── Raw SASL message codec ────────────────────────────────────────────────────

/// Decode `AuthenticationSASL` payload (sub=10) → list of offered mechanism names.
pub fn decode_sasl_mechanisms(payload: &[u8]) -> Result<Vec<String>> {
    if payload.len() < 4 {
        return Err(DbError::Protocol("AuthSASL payload too short".into()));
    }
    let mut mechanisms = Vec::new();
    let mut pos = 4; // skip the 4-byte sub-type field
    while pos < payload.len() {
        let start = pos;
        while pos < payload.len() && payload[pos] != 0 {
            pos += 1;
        }
        if pos == start {
            break; // empty string = list terminator
        }
        let name = std::str::from_utf8(&payload[start..pos])
            .map_err(|_| DbError::Protocol("non-UTF-8 SASL mechanism name".into()))?
            .to_string();
        mechanisms.push(name);
        pos += 1; // skip null terminator
    }
    Ok(mechanisms)
}

/// Decode `AuthenticationSASLContinue` payload (sub=11) → server-first-message string.
pub fn decode_sasl_continue(payload: &[u8]) -> Result<String> {
    if payload.len() < 4 {
        return Err(DbError::Protocol("AuthSASLContinue payload too short".into()));
    }
    std::str::from_utf8(&payload[4..])
        .map(|s| s.to_string())
        .map_err(|_| DbError::Protocol("non-UTF-8 SASLContinue data".into()))
}

/// Decode `AuthenticationSASLFinal` payload (sub=12) → server-final-message string.
pub fn decode_sasl_final(payload: &[u8]) -> Result<String> {
    if payload.len() < 4 {
        return Err(DbError::Protocol("AuthSASLFinal payload too short".into()));
    }
    std::str::from_utf8(&payload[4..])
        .map(|s| s.to_string())
        .map_err(|_| DbError::Protocol("non-UTF-8 SASLFinal data".into()))
}

/// Encode a `SASLInitialResponse` frontend message payload.
///
/// Format: `mechanism_name\0` + `int32(client_first_len)` + `client_first_bytes`
pub fn encode_sasl_initial_response(mechanism: &str, client_first: &str) -> Vec<u8> {
    let msg = client_first.as_bytes();
    let mut buf = Vec::with_capacity(mechanism.len() + 1 + 4 + msg.len());
    buf.extend_from_slice(mechanism.as_bytes());
    buf.push(0); // null terminator for mechanism name
    buf.extend_from_slice(&(msg.len() as i32).to_be_bytes());
    buf.extend_from_slice(msg);
    buf
}

/// Encode a `SASLResponse` frontend message payload (raw client-final-message bytes).
pub fn encode_sasl_response(client_final: &str) -> Vec<u8> {
    client_final.as_bytes().to_vec()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract a named attribute from a comma-separated SCRAM message.
///
/// SCRAM attributes are `key=value` pairs. Returns the value slice or `None`.
fn extract_attr<'a>(msg: &'a str, key: char) -> Option<&'a str> {
    let prefix = format!("{key}=");
    for part in msg.split(',') {
        if let Some(val) = part.strip_prefix(prefix.as_str()) {
            return Some(val);
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // ── PBKDF2 ────────────────────────────────────────────────────────────────

    /// RFC 6070 PBKDF2-SHA-256 vector: password="password", salt="salt", c=1
    #[test]
    fn pbkdf2_rfc6070_vector() {
        let result = pbkdf2_hmac_sha256(b"password", b"salt", 1);
        assert_eq!(
            hex(&result),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
    }

    // ── HMAC-SHA-256 ──────────────────────────────────────────────────────────

    /// RFC 4231 test vector #1
    #[test]
    fn hmac_sha256_rfc4231_vector1() {
        let result = hmac_sha256(&[0x0bu8; 20], b"Hi There");
        assert_eq!(
            hex(&result),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    /// RFC 4231 test vector #2: key="Jefe", data="what do ya want for nothing?"
    #[test]
    fn hmac_sha256_rfc4231_vector2() {
        let result = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&result),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    // ── SCRAM message helpers ─────────────────────────────────────────────────

    #[test]
    fn extract_attr_parses_correctly() {
        let msg = "r=combined-nonce,s=c2FsdA==,i=4096";
        assert_eq!(extract_attr(msg, 'r'), Some("combined-nonce"));
        assert_eq!(extract_attr(msg, 's'), Some("c2FsdA=="));
        assert_eq!(extract_attr(msg, 'i'), Some("4096"));
        assert_eq!(extract_attr(msg, 'x'), None);
    }

    #[test]
    fn client_first_message_format() {
        let client = ScramClient {
            username: "testuser".into(),
            password: "testpass".into(),
            client_nonce: "abc123".into(),
        };
        assert_eq!(client.client_first_message(), "n,,n=testuser,r=abc123");
        assert_eq!(client.client_first_bare(), "n=testuser,r=abc123");
    }

    /// Known-vector SCRAM-SHA-256 test from RFC 7677 §3.
    ///
    /// user="user", password="pencil",
    /// client-nonce="rOprNGfwEbeRWgbNEkqO",
    /// server-nonce="rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0",
    /// salt=W22ZaJ0SNY7soEsUEjb6gQ==, iterations=4096
    #[test]
    fn scram_sha256_rfc7677_known_vector() {
        let client = ScramClient {
            username: "user".into(),
            password: "pencil".into(),
            client_nonce: "rOprNGfwEbeRWgbNEkqO".into(),
        };

        let server_first =
            "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";

        let (client_final, server_sig) = client.process_server_first(server_first).unwrap();

        // Expected from RFC 7677 §3
        let expected_final =
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=";
        assert_eq!(client_final, expected_final);

        let expected_server_final = "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";
        client
            .verify_server_final(expected_server_final, &server_sig)
            .expect("server signature should verify");
    }

    #[test]
    fn scram_nonce_mismatch_returns_error() {
        let client = ScramClient {
            username: "user".into(),
            password: "pass".into(),
            client_nonce: "myclientnonce".into(),
        };
        let server_first = "r=WRONGNONCE,s=c2FsdA==,i=4096";
        assert!(client.process_server_first(server_first).is_err());
    }

    #[test]
    fn decode_sasl_mechanisms_parses_list() {
        let mut payload = 10i32.to_be_bytes().to_vec();
        payload.extend_from_slice(b"SCRAM-SHA-256\0\0");
        let mechanisms = decode_sasl_mechanisms(&payload).unwrap();
        assert_eq!(mechanisms, vec!["SCRAM-SHA-256"]);
    }

    #[test]
    fn encode_sasl_initial_response_layout() {
        let buf = encode_sasl_initial_response("SCRAM-SHA-256", "n,,n=user,r=nonce");
        assert_eq!(&buf[..14], b"SCRAM-SHA-256\0");
        let msg_len = i32::from_be_bytes(buf[14..18].try_into().unwrap());
        assert_eq!(msg_len as usize, "n,,n=user,r=nonce".len());
        assert_eq!(&buf[18..], b"n,,n=user,r=nonce");
    }

    #[test]
    fn generate_nonce_is_base64_and_nonempty() {
        let nonce = generate_nonce();
        assert!(!nonce.is_empty());
        // Must be decodable as Base64
        Base64::decode_vec(&nonce).expect("nonce should be valid base64");
    }
}
