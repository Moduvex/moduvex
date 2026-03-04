//! SHA-256 test coverage — verifies the `sha2` crate against NIST FIPS 180-4 vectors.
//!
//! The `sha2` crate (RustCrypto, same ecosystem as `md-5`) is used throughout
//! this module for SHA-256 computation. These tests ensure the dependency
//! behaves correctly for our use case.

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn sha256(data: &[u8]) -> Vec<u8> {
        Sha256::digest(data).to_vec()
    }

    /// NIST FIPS 180-4 Appendix B.1: SHA-256("abc")
    #[test]
    fn sha256_abc() {
        // Direct call to sha2 crate — no intermediate function
        use sha2::Digest as Sha2Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"abc");
        let result: [u8; 32] = hasher.finalize().into();
        eprintln!("Direct sha2::Sha256 result: {}", hex(&result));

        // Also test via digest() method
        let result2: Vec<u8> = sha2::Sha256::digest(b"abc").to_vec();
        eprintln!("Via digest(): {}", hex(&result2));

        assert_eq!(
            hex(&result),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// NIST FIPS 180-4 Appendix B.1: SHA-256("") (empty string)
    #[test]
    fn sha256_empty() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// NIST FIPS 180-4 Appendix B.2: 448-bit message
    #[test]
    fn sha256_448bit_message() {
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
