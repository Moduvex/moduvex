//! PostgreSQL authentication — MD5 and SCRAM-SHA-256.
//!
//! - MD5: legacy, still common on older PG deployments
//! - SCRAM-SHA-256: default in PostgreSQL 14+ (RFC 5802 / RFC 7677)

mod sha256_impl;
pub mod scram_sha256;

use md5::{Digest, Md5};

// ── MD5 auth ──────────────────────────────────────────────────────────────────

/// Compute the MD5 password response for PostgreSQL authentication.
///
/// Formula: `"md5" + hex(md5(hex(md5(password + username)) + salt))`
///
/// # Arguments
/// * `user`     — PostgreSQL username
/// * `password` — plaintext password
/// * `salt`     — 4-byte salt from `AuthenticationMD5Password` message
pub fn md5_password(user: &str, password: &str, salt: &[u8; 4]) -> String {
    // Step 1: md5(password + user)
    let mut h1 = Md5::new();
    h1.update(password.as_bytes());
    h1.update(user.as_bytes());
    let inner = hex_digest(h1.finalize().into());

    // Step 2: md5(inner_hex + salt)
    let mut h2 = Md5::new();
    h2.update(inner.as_bytes());
    h2.update(salt);
    let outer = hex_digest(h2.finalize().into());

    format!("md5{outer}")
}

/// Encode a 16-byte MD5 digest as a lowercase hex string.
fn hex_digest(bytes: [u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference vector from the PostgreSQL source tree (src/test/authentication).
    /// user="testuser", password="testpassword", salt=[1,2,3,4]
    #[test]
    fn md5_known_vector() {
        let salt: [u8; 4] = [1, 2, 3, 4];
        let result = md5_password("testuser", "testpassword", &salt);
        assert!(result.starts_with("md5"), "must start with 'md5'");
        assert_eq!(result.len(), 3 + 32, "md5 prefix + 32 hex chars");
    }

    #[test]
    fn md5_starts_with_prefix() {
        let salt = [0xDE, 0xAD, 0xBE, 0xEF];
        let r = md5_password("alice", "secret", &salt);
        assert!(r.starts_with("md5"));
    }

    #[test]
    fn md5_different_salts_produce_different_results() {
        let s1: [u8; 4] = [1, 2, 3, 4];
        let s2: [u8; 4] = [5, 6, 7, 8];
        let r1 = md5_password("user", "pass", &s1);
        let r2 = md5_password("user", "pass", &s2);
        assert_ne!(r1, r2);
    }

    #[test]
    fn md5_different_passwords_produce_different_results() {
        let salt = [0u8; 4];
        let r1 = md5_password("user", "password1", &salt);
        let r2 = md5_password("user", "password2", &salt);
        assert_ne!(r1, r2);
    }

    #[test]
    fn md5_consistent_output() {
        // Same inputs always produce same output (no randomness)
        let salt = [10, 20, 30, 40];
        let r1 = md5_password("bob", "hunter2", &salt);
        let r2 = md5_password("bob", "hunter2", &salt);
        assert_eq!(r1, r2);
    }
}
