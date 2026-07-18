//! Ported from RSCTF `Utils/Codec.cs` — encoding and hashing helpers.

use base64::Engine;
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of arbitrary bytes.
pub fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

/// SHA-256 of a UTF-8 string (`string.ToSHA256String()` in RSCTF).
pub fn sha256_str(input: &str) -> String {
    sha256_hex(input.as_bytes())
}

/// `n` bytes of CSPRNG entropy.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::fill(&mut buf);
    buf
}

/// `n` bytes of entropy as lowercase hex.
pub fn random_hex(n: usize) -> String {
    hex::encode(random_bytes(n))
}

/// `n` bytes of entropy as URL-safe base64 (opaque tokens).
pub fn random_token(n: usize) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(n))
}

pub fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}
