//! Event-capability rotation, cooldown, and upgrade-safe roster repair.

use std::collections::BTreeSet;

use sqlx::{Postgres, QueryBuilder};

use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

/// One full scoring cadence. A player gets an immediate first emergency
/// rotation, then cannot churn the shared reporter context or stability fence
/// more than once per cadence.
pub(crate) const PLAYER_API_TOKEN_ROTATION_COOLDOWN_SECONDS: i64 = 60;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlayerApiTokenRotation {
    Rotated(String),
    Cooldown { retry_after_seconds: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct ApiCapabilityIdentity {
    pub(crate) challenge_id: i32,
    pub(crate) participation_id: i32,
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct PendingApiCapability {
    challenge_id: i32,
    participation_id: i32,
}

fn fresh_api_token() -> String {
    format!("koth_{}", crate::utils::codec::random_token(18))
}

/// Record a durable security fence in the caller's mutation transaction.
/// Authentication and reporter context fail closed until reconciliation
/// clears the fence.
pub(crate) async fn request_event_capability_revocation(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    participation_ids: &[i32],
) -> AppResult<BTreeSet<i32>> {
    if participation_ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    sqlx::query_scalar(
        r#"UPDATE "KothApiTeamTokens"
              SET revocation_pending = TRUE
            WHERE game_id = $1 AND participation_id = ANY($2)
            RETURNING challenge_id"#,
    )
    .bind(game_id)
    .bind(participation_ids)
    .fetch_all(&mut *connection)
    .await
    .map(|rows| rows.into_iter().collect())
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Apply each requested security fence exactly once. The bearer digest is
/// also the arena identity, so changing the secret invalidates pre-revocation
/// external evidence. Redaction, Crown repair, snapshot fencing, and the
/// applied-generation write share the same transaction.
const RECONCILE_PAGE_SIZE: usize = 1_000;
const MAX_RECONCILED_CAPABILITIES_PER_TRANSACTION: usize = 32_000;

async fn reconcile_pending_event_capabilities_batch(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: Option<i32>,
    participation_ids: Option<&[i32]>,
) -> AppResult<Vec<ApiCapabilityIdentity>> {
    let pending: Vec<PendingApiCapability> = sqlx::query_as(
        r#"SELECT challenge_id, participation_id
             FROM "KothApiTeamTokens"
            WHERE game_id = $1
              AND revocation_pending
              AND ($2::integer IS NULL OR challenge_id = $2)
              AND ($3::integer[] IS NULL OR participation_id = ANY($3))
            ORDER BY challenge_id, participation_id
            LIMIT 1000
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(participation_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    let mut issued = BTreeSet::new();
    let rows: Vec<_> = pending
        .iter()
        .map(|capability| {
            let token = loop {
                let candidate = fresh_api_token();
                if issued.insert(candidate.clone()) {
                    break candidate;
                }
            };
            (capability.challenge_id, capability.participation_id, token)
        })
        .collect();
    let mut update = QueryBuilder::<Postgres>::new(
        r#"UPDATE "KothApiTeamTokens" capability
              SET token = rotated.token,
                  generation = capability.generation + 1,
                  rotated_at = clock_timestamp(),
                  last_used_at = NULL
             FROM ("#,
    );
    update.push_values(
        &rows,
        |mut values, (challenge_id, participation_id, token)| {
            values
                .push_bind(*challenge_id)
                .push_bind(*participation_id)
                .push_bind(token);
        },
    );
    update.push(
        r#") AS rotated(challenge_id, participation_id, token)
            WHERE capability.game_id = "#,
    );
    update.push_bind(game_id);
    update.push(
        r#" AND capability.challenge_id = rotated.challenge_id
             AND capability.participation_id = rotated.participation_id"#,
    );
    let updated = update
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if updated != pending.len() as u64 {
        return Err(AppError::internal(
            "Leaderboard capability security rotation was incomplete",
        ));
    }

    let affected_challenges: BTreeSet<_> = pending
        .iter()
        .map(|capability| capability.challenge_id)
        .collect();
    for affected_challenge in affected_challenges {
        let affected_participations: Vec<_> = pending
            .iter()
            .filter(|capability| capability.challenge_id == affected_challenge)
            .map(|capability| capability.participation_id)
            .collect();
        super::clear_unsettled_scores_for_capability_change(
            connection,
            game_id,
            affected_challenge,
            &affected_participations,
        )
        .await?;
    }

    let mut applied = QueryBuilder::<Postgres>::new(
        r#"UPDATE "KothApiTeamTokens" capability
              SET revocation_pending = FALSE
             FROM ("#,
    );
    applied.push_values(&pending, |mut values, capability| {
        values
            .push_bind(capability.challenge_id)
            .push_bind(capability.participation_id);
    });
    applied.push(
        r#") AS requested(challenge_id, participation_id)
            WHERE capability.game_id = "#,
    );
    applied.push_bind(game_id);
    applied.push(
        r#" AND capability.challenge_id = requested.challenge_id
             AND capability.participation_id = requested.participation_id
             AND capability.revocation_pending"#,
    );
    let updated = applied
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if updated != pending.len() as u64 {
        return Err(AppError::internal(
            "Leaderboard capability revocation fence advanced incompletely",
        ));
    }
    Ok(pending
        .into_iter()
        .map(|capability| ApiCapabilityIdentity {
            challenge_id: capability.challenge_id,
            participation_id: capability.participation_id,
        })
        .collect())
}

/// Reconcile a bounded event field without exceeding PostgreSQL's bind limit.
/// The platform manifest bounds a Leaderboard roster to 2,000 teams; the
/// explicit total bound also keeps a malformed legacy event fail closed.
pub(crate) async fn reconcile_pending_event_capabilities(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: Option<i32>,
    participation_ids: Option<&[i32]>,
) -> AppResult<Vec<ApiCapabilityIdentity>> {
    let mut reconciled = Vec::new();
    loop {
        let batch = reconcile_pending_event_capabilities_batch(
            connection,
            game_id,
            challenge_id,
            participation_ids,
        )
        .await?;
        let batch_len = batch.len();
        if reconciled.len().saturating_add(batch_len) > MAX_RECONCILED_CAPABILITIES_PER_TRANSACTION
        {
            return Err(AppError::conflict(
                "Leaderboard capability revocation field exceeds the supported bound",
            ));
        }
        reconciled.extend(batch);
        if batch_len < RECONCILE_PAGE_SIZE {
            return Ok(reconciled);
        }
    }
}

/// Security revocation preserves each event-capability row so a later
/// eligibility restoration is immediately complete, but replaces every secret
/// in the affected `(challenge, participation)` set. Callers already own the
/// per-game transaction lock. Returned values are challenge identifiers only;
/// freshly issued bearer secrets never leave this service boundary.
pub(crate) async fn force_rotate_event_capabilities(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    participation_ids: &[i32],
) -> AppResult<BTreeSet<i32>> {
    let requested =
        request_event_capability_revocation(connection, game_id, participation_ids).await?;
    reconcile_pending_event_capabilities(connection, game_id, None, Some(participation_ids))
        .await?;
    Ok(requested)
}

/// Atomically perform one player-requested emergency rotation. A capability
/// without a prior player rotation may rotate immediately; every later player
/// rotation observes the per-row cooldown. Security/admin revocation uses
/// [`force_rotate_event_capabilities`] without consuming or resetting this
/// player-only cooldown.
pub(crate) async fn rotate_player_api_capability(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
) -> AppResult<PlayerApiTokenRotation> {
    let candidate = fresh_api_token();
    let rotated = sqlx::query_scalar(
        r#"INSERT INTO "KothApiTeamTokens"
                   (game_id, challenge_id, participation_id, token, generation,
                    last_player_rotated_at)
               VALUES ($1, $2, $3, $4, 2, clock_timestamp())
               ON CONFLICT (game_id, challenge_id, participation_id) DO UPDATE
                 SET token = EXCLUDED.token,
                     generation = "KothApiTeamTokens".generation + 1,
                     rotated_at = clock_timestamp(),
                     last_player_rotated_at = clock_timestamp(),
                     last_used_at = NULL
               WHERE NOT "KothApiTeamTokens".revocation_pending
                 AND (
                     "KothApiTeamTokens".last_player_rotated_at IS NULL
                     OR "KothApiTeamTokens".last_player_rotated_at
                          <= clock_timestamp()
                             - ($5::bigint * interval '1 second')
                 )
               RETURNING token"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(participation_id)
    .bind(candidate)
    .bind(PLAYER_API_TOKEN_ROTATION_COOLDOWN_SECONDS)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(token) = rotated {
        return Ok(PlayerApiTokenRotation::Rotated(token));
    }

    let retry_after: i64 = sqlx::query_scalar(
        r#"SELECT GREATEST(
                    1,
                    CEIL(EXTRACT(EPOCH FROM (
                        last_player_rotated_at
                        + ($4::bigint * interval '1 second')
                        - clock_timestamp()
                    )))::bigint
                )
             FROM "KothApiTeamTokens"
            WHERE game_id = $1 AND challenge_id = $2
              AND participation_id = $3
              AND NOT revocation_pending
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(participation_id)
    .bind(PLAYER_API_TOKEN_ROTATION_COOLDOWN_SECONDS)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::internal("Leaderboard capability disappeared during rotation"))?;
    Ok(PlayerApiTokenRotation::Cooldown {
        retry_after_seconds: u64::try_from(retry_after).unwrap_or(1),
    })
}

/// Repair rows deleted by older releases. Each page is bounded, uses only the
/// frozen official API hills and roster, and rechecks current eligibility. The
/// caller owns the game transaction lock. Every repaired challenge advances
/// the stored snapshot fence before commit.
pub(crate) async fn repair_missing_eligible_event_capabilities(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Vec<ApiCapabilityIdentity>> {
    const REPAIR_PAGE_SIZE: i64 = 1_000;
    const MAX_REPAIRED_CAPABILITIES_PER_TRANSACTION: usize =
        crate::services::ad::engine::koth_api::MAX_LEADERBOARD_TEAMS;
    const MAX_NO_PROGRESS_PASSES: usize = 3;

    let mut repaired = Vec::new();
    let mut no_progress = 0usize;
    loop {
        let missing: Vec<ApiCapabilityIdentity> = sqlx::query_as(
            r#"SELECT DISTINCT
                      (hill.item->>'challengeId')::integer AS challenge_id,
                      participation.id AS participation_id
                 FROM "KothOfficialConfigs" config
                 JOIN LATERAL jsonb_array_elements(config.hills_snapshot)
                        hill(item)
                   ON COALESCE(NULLIF(hill.item->>'claimSource', ''), 'Marker') = 'Api'
                 JOIN LATERAL jsonb_array_elements(config.roster_snapshot)
                        roster(item) ON TRUE
                 JOIN "Participations" participation
                   ON participation.id = CASE jsonb_typeof(roster.item)
                        WHEN 'number' THEN (roster.item #>> '{}')::integer
                        WHEN 'object' THEN
                          NULLIF(roster.item->>'participationId', '')::integer
                        ELSE NULL
                      END
                  AND participation.game_id = config.game_id
                  AND participation.status = $3
                 JOIN "Teams" team ON team.id = participation.team_id
                WHERE config.game_id = $1
                  AND (hill.item->>'challengeId')::integer = $2
                  AND NOT team.deletion_pending
                  AND EXISTS (
                      SELECT 1 FROM "KothCrownCycles" cycle
                       WHERE cycle.game_id = config.game_id
                         AND cycle.challenge_id =
                               (hill.item->>'challengeId')::integer
                         AND cycle.activated_at IS NOT NULL
                  )
                  AND NOT EXISTS (
                      SELECT 1
                        FROM (
                            SELECT team.captain_id AS user_id
                            UNION
                            SELECT member.user_id
                              FROM "TeamMembers" member
                             WHERE member.team_id = team.id
                        ) roster_member
                        LEFT JOIN "AspNetUsers" account
                          ON account.id = roster_member.user_id
                       WHERE account.id IS NULL OR account.role = $4
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM "KothApiTeamTokens" token
                       WHERE token.game_id = config.game_id
                         AND token.challenge_id =
                               (hill.item->>'challengeId')::integer
                         AND token.participation_id = participation.id
                  )
                ORDER BY challenge_id, participation_id
                LIMIT $5"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(Role::Banned as i16)
        .bind(REPAIR_PAGE_SIZE)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if missing.is_empty() {
            break;
        }

        let mut issued = BTreeSet::new();
        let rows: Vec<_> = missing
            .iter()
            .map(|identity| {
                let token = loop {
                    let candidate = fresh_api_token();
                    if issued.insert(candidate.clone()) {
                        break candidate;
                    }
                };
                (identity.challenge_id, identity.participation_id, token)
            })
            .collect();
        let mut insert = QueryBuilder::<Postgres>::new(
            r#"INSERT INTO "KothApiTeamTokens"
                    (game_id, challenge_id, participation_id, token)
               SELECT "#,
        );
        insert.push_bind(game_id);
        insert.push(", minted.challenge_id, minted.participation_id, minted.token FROM (");
        insert.push_values(
            &rows,
            |mut values, (challenge_id, participation_id, token)| {
                values
                    .push_bind(*challenge_id)
                    .push_bind(*participation_id)
                    .push_bind(token);
            },
        );
        insert.push(
            r#") AS minted(challenge_id, participation_id, token)
               ON CONFLICT DO NOTHING
               RETURNING challenge_id, participation_id"#,
        );
        let inserted: Vec<ApiCapabilityIdentity> = insert
            .build_query_as()
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if inserted.is_empty() {
            no_progress += 1;
            if no_progress >= MAX_NO_PROGRESS_PASSES {
                return Err(AppError::internal(
                    "Leaderboard capability roster repair made no progress",
                ));
            }
        } else {
            no_progress = 0;
            if repaired.len().saturating_add(inserted.len())
                > MAX_REPAIRED_CAPABILITIES_PER_TRANSACTION
            {
                return Err(AppError::conflict(
                    "Leaderboard capability repair field exceeds the supported bound",
                ));
            }
            repaired.extend(inserted);
        }
    }

    let affected_challenges: BTreeSet<_> = repaired
        .iter()
        .map(|identity| identity.challenge_id)
        .collect();
    for challenge_id in affected_challenges {
        super::invalidate_unsettled_snapshot_versions(connection, game_id, challenge_id).await?;
    }
    Ok(repaired)
}

#[cfg(test)]
mod tests;
