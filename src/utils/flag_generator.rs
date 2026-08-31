//! Ported from RSCTF `Utils/FlagGenerator.cs` — dynamic flag derivation.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::utils::codec::{random_hex, sha256_str};
use crate::utils::error::{AppError, AppResult};

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

/// Generate a practice/Jeopardy flag that the canonical submit endpoint can
/// actually receive. This runtime backstop fails closed for legacy oversized
/// templates instead of launching a container with an impossible answer.
pub fn generate_flag_checked(template: Option<&str>, team_hash: &str) -> AppResult<String> {
    let flag = generate_flag(template, team_hash);
    if flag.as_bytes().len() > crate::controllers::game::MAX_FLAG_LENGTH {
        tracing::warn!(
            produced_bytes = flag.as_bytes().len(),
            "generated flag exceeds the player submission envelope"
        );
        return Err(AppError::unavailable(
            "Challenge generated an invalid flag; ask an administrator to repair it",
        ));
    }
    Ok(flag)
}

/// Expand a flag template deterministically for one crash-retryable workload.
/// The team hash keeps the result secret even though an idempotency key may be
/// visible to an administrator or transport log.
pub fn generate_retryable_flag(
    template: Option<&str>,
    team_hash: &str,
    operation_id: &str,
) -> String {
    let derive = |domain: &str| {
        sha256_str(&format!(
            "rsctf:retryable-flag:v1\0{domain}\0{team_hash}\0{operation_id}"
        ))
    };
    let guid = deterministic_uuid(&derive("guid"));
    let uuid = deterministic_uuid(&derive("uuid"));
    match template {
        None | Some("") => format!("flag{{{}}}", &derive("empty")[..32]),
        Some(template) => template
            .replace("[GUID]", &guid)
            .replace("[UUID]", &uuid)
            .replace("[TEAM_HASH]", &team_hash[..team_hash.len().min(16)]),
    }
}

/// Deterministic counterpart of [`generate_flag_checked`] for a runtime whose
/// create may be adopted after a lost response or process restart.
pub fn generate_retryable_flag_checked(
    template: Option<&str>,
    team_hash: &str,
    operation_id: &str,
) -> AppResult<String> {
    let flag = generate_retryable_flag(template, team_hash, operation_id);
    if flag.as_bytes().len() > crate::controllers::game::MAX_FLAG_LENGTH {
        tracing::warn!(
            produced_bytes = flag.as_bytes().len(),
            "generated retryable flag exceeds the player submission envelope"
        );
        return Err(AppError::unavailable(
            "Challenge generated an invalid flag; ask an administrator to repair it",
        ));
    }
    Ok(flag)
}

fn deterministic_uuid(digest: &str) -> String {
    let mut bytes = [0u8; 16];
    hex::decode_to_slice(&digest[..32], &mut bytes)
        .expect("a SHA-256 digest always contains 16 bytes of hexadecimal data");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
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

    #[test]
    fn retryable_flags_are_stable_only_for_the_same_workload_identity() {
        let template = Some("flag{[TEAM_HASH]-[GUID]-[UUID]}");
        let first = generate_retryable_flag(template, "team-secret-hash", "operation-1");

        assert_eq!(
            first,
            generate_retryable_flag(template, "team-secret-hash", "operation-1")
        );
        assert_ne!(
            first,
            generate_retryable_flag(template, "team-secret-hash", "operation-2")
        );
        assert_ne!(
            first,
            generate_retryable_flag(template, "other-team-hash", "operation-1")
        );
        assert_eq!(
            generate_retryable_flag(None, "team-secret-hash", "operation-1"),
            generate_retryable_flag(Some(""), "team-secret-hash", "operation-1")
        );
    }

    #[test]
    fn checked_generation_rejects_an_unsubmittable_expansion() {
        let template = format!("flag{{{}}}", "[GUID]".repeat(4));
        assert!(generate_flag_checked(Some(&template), &"a".repeat(64)).is_err());
        assert!(generate_flag_checked(Some("flag{[TEAM_HASH]}"), &"b".repeat(64)).is_ok());
        assert!(
            generate_retryable_flag_checked(Some(&template), &"a".repeat(64), "operation",)
                .is_err()
        );
    }
}
