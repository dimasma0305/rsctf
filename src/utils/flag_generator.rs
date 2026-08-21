//! Ported from RSCTF `Utils/FlagGenerator.cs` — dynamic flag derivation.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
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

/// Secret, deterministic per-(exercise,user) seed for standalone exercise flags.
/// The domain prefix prevents reuse of the deployment identity key from making
/// this value collide with any other keyed identity derived by rsctf.
pub fn exercise_user_hash(secret: &[u8], exercise_id: i32, user_id: Uuid) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"rsctf:exercise-flag:v1\0");
    mac.update(&exercise_id.to_be_bytes());
    mac.update(user_id.as_bytes());
    hex::encode(mac.finalize().into_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exercise_hash_is_secret_deterministic_and_identity_scoped() {
        let user = Uuid::parse_str("018f0000-0000-7000-8000-000000000001").unwrap();
        let first = exercise_user_hash(b"first deployment secret", 7, user);

        assert_eq!(
            first,
            exercise_user_hash(b"first deployment secret", 7, user)
        );
        assert_ne!(
            first,
            exercise_user_hash(b"second deployment secret", 7, user)
        );
        assert_ne!(
            first,
            exercise_user_hash(b"first deployment secret", 8, user)
        );
        assert_ne!(
            first,
            exercise_user_hash(
                b"first deployment secret",
                7,
                Uuid::parse_str("018f0000-0000-7000-8000-000000000002").unwrap()
            )
        );
    }
}
