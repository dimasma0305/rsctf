//! Authentication primitives for participation-scoped A&D automation tokens.
//!
//! The HTTP limiter and the player controllers both need the same credential
//! decision. Keeping it here avoids duplicating the security rules or making a
//! middleware depend on a controller module.

use sha2::{Digest, Sha256};

use crate::models::data::participation;
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};

pub const PREFIX: &str = "ad_";
const ENCODED_SECRET_LEN: usize = 43;

/// A token that was resolved against the current database roster.
#[derive(Clone, Debug)]
pub struct VerifiedTeamToken {
    pub participation: participation::Model,
    /// Fixed-size, non-secret limiter identity. This is the persisted SHA-256
    /// digest, namespaced so it cannot collide with a session identity.
    pub partition_key: String,
}

/// Request marker set after the global middleware has already queried a
/// syntactically valid A&D bearer token and found no current credential. Dual-
/// auth handlers treat this as a terminal 401 instead of repeating the same DB
/// lookup during extraction.
#[derive(Clone, Copy, Debug)]
pub struct RejectedTeamToken;

/// Return a borrowed bearer credential without allocating.
pub fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Exact public shape emitted by the token generator. Reject malformed `ad_`
/// candidates before hashing or touching PostgreSQL.
pub fn is_well_formed(token: &str) -> bool {
    let Some(secret) = token.strip_prefix(PREFIX) else {
        return false;
    };
    secret.len() == ENCODED_SECRET_LEN
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// SHA-256 hex persisted in `AdTeamApiTokens.token_hash`.
pub fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Resolve one A&D credential in a single database round trip.
///
/// The materialized candidate validates the token, accepted participation,
/// owning team, and every current team member (including the captain). The DML
/// CTE updates usage at most once per 30 seconds, atomically across replicas;
/// ordinary polling therefore performs no write and takes no row lock.
pub async fn authenticate(
    pool: &sqlx::PgPool,
    token: &str,
) -> AppResult<Option<VerifiedTeamToken>> {
    if !is_well_formed(token) {
        return Ok(None);
    }
    let token_hash = hash(token);
    type Row = (i32, String, Option<i32>, i32, i32, Option<i32>, i32, bool);
    let row = sqlx::query_as::<_, Row>(
        r#"WITH candidate AS MATERIALIZED (
             SELECT credential.id AS credential_id,
                    participation.id,
                    participation.token,
                    participation.writeup_id,
                    participation.game_id,
                    participation.team_id,
                    participation.division_id,
                    participation.suspicion_score
               FROM "AdTeamApiTokens" credential
               JOIN "Participations" participation
                 ON participation.id = credential.participation_id
                AND participation.status = $2
               JOIN "Teams" team ON team.id = participation.team_id
              WHERE credential.token_hash = $1
                AND NOT EXISTS (
                    SELECT 1
                      FROM (
                          SELECT team.captain_id AS user_id
                          UNION
                          SELECT member.user_id
                            FROM "TeamMembers" member
                           WHERE member.team_id = team.id
                      ) roster
                      LEFT JOIN "AspNetUsers" account ON account.id = roster.user_id
                     WHERE account.id IS NULL OR account.role = 0
                )
              LIMIT 1
           ), usage_update AS (
             UPDATE "AdTeamApiTokens" credential
                SET last_used_at_utc = now()
               FROM candidate
              WHERE credential.id = candidate.credential_id
                AND (
                    credential.last_used_at_utc IS NULL
                    OR credential.last_used_at_utc < now() - interval '30 seconds'
                )
              RETURNING credential.id
           )
           SELECT candidate.id,
                  candidate.token,
                  candidate.writeup_id,
                  candidate.game_id,
                  candidate.team_id,
                  candidate.division_id,
                  candidate.suspicion_score,
                  EXISTS(SELECT 1 FROM usage_update)
             FROM candidate"#,
    )
    .bind(&token_hash)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(row.map(
        |(id, token, writeup_id, game_id, team_id, division_id, suspicion_score, _updated)| {
            VerifiedTeamToken {
                participation: participation::Model {
                    id,
                    status: ParticipationStatus::Accepted,
                    token,
                    writeup_id,
                    game_id,
                    team_id,
                    division_id,
                    suspicion_score,
                },
                partition_key: format!("ad:{token_hash}"),
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_generated_token_shape() {
        let token = format!("{PREFIX}{}", "a".repeat(ENCODED_SECRET_LEN));
        assert!(is_well_formed(&token));
        assert!(is_well_formed(&format!(
            "{PREFIX}{}",
            "-_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcde"
        )));
        assert!(!is_well_formed("ad_short"));
        assert!(!is_well_formed(&format!("{PREFIX}{}=", "a".repeat(42))));
        assert!(!is_well_formed(&format!("{PREFIX}{}", "a".repeat(44))));
        assert!(!is_well_formed(&format!(
            "{PREFIX}{}",
            "a".repeat(42) + "/"
        )));
    }

    #[test]
    fn hash_is_fixed_size_and_namespaced_at_use() {
        let digest = hash(&format!("{PREFIX}{}", "a".repeat(ENCODED_SECRET_LEN)));
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
