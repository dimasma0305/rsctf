//! Ported from RSCTF `Utils/FlagGenerator.cs` — dynamic flag derivation.

use uuid::Uuid;

use crate::utils::codec::{random_hex, sha256_str};

/// Per-game team salt: `SHA256("RSCTF@{private_key}@PK")`.
pub fn team_hash_salt(private_key: &str) -> String {
    sha256_str(&format!("RSCTF@{private_key}@PK"))
}

/// Deterministic per-(team,challenge) hash used to seed dynamic flags.
pub fn team_challenge_hash(salt: &str, challenge_id: i32, team_token: &str) -> String {
    sha256_str(&format!("{salt}::{challenge_id}::{team_token}"))
}

/// Expand a flag template's placeholders. Supports `[GUID]`, `[UUID]`, and
/// `[TEAM_HASH]`; an empty template yields a random `flag{...}`.
pub fn generate_flag(template: Option<&str>, team_hash: &str) -> String {
    match template {
        None | Some("") => format!("flag{{{}}}", random_hex(16)),
        Some(t) => t
            .replace("[GUID]", &Uuid::new_v4().to_string())
            .replace("[UUID]", &Uuid::new_v4().to_string())
            .replace("[TEAM_HASH]", &team_hash[..team_hash.len().min(16)]),
    }
}
