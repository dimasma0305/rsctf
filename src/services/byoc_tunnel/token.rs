//! Stable BYOC token derivation shared by HTTP admission and tunnel authorization.

use sha2::{Digest, Sha256};

/// Deterministic per-`(participation, challenge)` BYOC token, hex-encoded.
/// `domain` domain-separates the agent/image tokens (mirrors RSCTF's
/// `AdTokenUtils.ByocAgentToken` / `ByocImageToken` prefixes).
pub(crate) fn byoc_token(
    domain: &str,
    game_secret: &str,
    team_secret: &str,
    participation_id: i32,
    challenge_id: i32,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(game_secret.as_bytes());
    hasher.update(team_secret.as_bytes());
    hasher.update(participation_id.to_le_bytes());
    hasher.update(challenge_id.to_le_bytes());
    hex::encode(hasher.finalize())
}
