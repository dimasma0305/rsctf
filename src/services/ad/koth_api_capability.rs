//! Event-scoped player authentication for Leaderboard/API KotH arenas.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use sqlx::{Postgres, QueryBuilder};

use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

mod rotation;

pub(crate) use rotation::{
    force_rotate_event_capabilities, reconcile_pending_event_capabilities,
    repair_missing_eligible_event_capabilities, request_event_capability_revocation,
    rotate_player_api_capability, PlayerApiTokenRotation,
};

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct AuthenticatedApiTeam {
    pub(crate) game_id: i32,
    pub(crate) challenge_id: i32,
    pub(crate) participation_id: i32,
    pub(crate) team_name: String,
}

/// Reject attacker-sized or ambiguous values before looking up a capability.
pub(crate) fn is_well_formed(token: &str) -> bool {
    let Some(secret) = token.strip_prefix("koth_") else {
        return false;
    };
    (8..=128).contains(&secret.len())
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub(crate) fn token_hash_hex(token: &str) -> String {
    hex::encode(token_hash(token))
}

#[derive(Debug, sqlx::FromRow)]
struct RemovedScoreWave {
    target_id: i32,
    wave_id: String,
}

fn next_snapshot_version(value: &[u8]) -> AppResult<[u8; 32]> {
    let mut next: [u8; 32] = value
        .try_into()
        .map_err(|_| AppError::internal("Leaderboard snapshot has an invalid digest"))?;
    for byte in next.iter_mut().rev() {
        let (incremented, overflow) = byte.overflowing_add(1);
        *byte = incremented;
        if !overflow {
            break;
        }
    }
    Ok(next)
}

async fn bump_snapshot_versions(
    connection: &mut sqlx::PgConnection,
    snapshots: Vec<(i32, Vec<u8>)>,
) -> AppResult<u64> {
    if snapshots.is_empty() {
        return Ok(0);
    }
    let bumped: Vec<_> = snapshots
        .into_iter()
        .map(|(target_id, hash)| Ok((target_id, next_snapshot_version(&hash)?)))
        .collect::<AppResult<_>>()?;
    let mut query = QueryBuilder::<Postgres>::new(
        r#"UPDATE "KothApiSnapshots" snapshot
              SET snapshot_hash = bumped.snapshot_hash
             FROM ("#,
    );
    query.push_values(&bumped, |mut values, (target_id, hash)| {
        values.push_bind(*target_id).push_bind(hash.as_slice());
    });
    query.push(
        r#") AS bumped(target_id, snapshot_hash)
            WHERE snapshot.target_id = bumped.target_id"#,
    );
    let updated = query
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if updated != bumped.len() as u64 {
        return Err(AppError::internal(
            "Leaderboard snapshot version update was incomplete",
        ));
    }
    Ok(updated)
}

/// Remove revoked teams from unsettled Leaderboard evidence while preserving
/// every other team's rows. Crown ownership is then derived again from the
/// remaining completed, positive evidence using exact ratio comparisons.
///
/// The caller owns the surrounding capability transaction and game lock. The
/// snapshot hash is also an optimistic version fence for the functional
/// checker, so every affected parent receives a distinct next 256-bit version
/// in the same transaction. A later referee submission restores its canonical
/// content digest.
async fn clear_unsettled_scores_inner(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: Option<i32>,
    target_id: Option<i32>,
    participation_ids: &[i32],
    fence_capability_change: bool,
) -> AppResult<u64> {
    if participation_ids.is_empty() {
        return Ok(0);
    }
    // Match reporter submission's parent-before-children lock order. Manual
    // rotation and game revocation enter after changing capability rows; an
    // observation rebase is already serialized by its locked reporter. Every
    // path therefore converges on snapshot, then score rows in that order.
    let snapshots: Vec<(i32, Vec<u8>)> = sqlx::query_as(
        r#"SELECT snapshot.target_id, snapshot.snapshot_hash
             FROM "KothApiSnapshots" snapshot
            WHERE snapshot.game_id = $1
              AND ($2::integer IS NULL OR snapshot.challenge_id = $2)
              AND ($3::integer IS NULL OR snapshot.target_id = $3)
              AND ($5 OR EXISTS (
                    SELECT 1 FROM "KothApiSnapshotScores" score
                     WHERE score.target_id = snapshot.target_id
                       AND score.participation_id = ANY($4)
              ))
            ORDER BY snapshot.target_id
            FOR UPDATE OF snapshot"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(target_id)
    .bind(participation_ids)
    .bind(fence_capability_change)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if snapshots.is_empty() {
        return Ok(0);
    }
    let locked_target_ids: Vec<_> = snapshots.iter().map(|(target_id, _)| *target_id).collect();
    let removed = sqlx::query_as::<_, RemovedScoreWave>(
        r#"DELETE FROM "KothApiSnapshotScores" score
            WHERE score.target_id = ANY($1)
              AND score.participation_id = ANY($2)
            RETURNING score.target_id, score.wave_id"#,
    )
    .bind(&locked_target_ids)
    .bind(participation_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if removed.is_empty() {
        if fence_capability_change {
            bump_snapshot_versions(connection, snapshots).await?;
        }
        return Ok(0);
    }

    let affected_waves: BTreeSet<_> = removed
        .iter()
        .map(|row| (row.target_id, row.wave_id.clone()))
        .collect();
    let target_ids: Vec<_> = affected_waves
        .iter()
        .map(|(target_id, _)| *target_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let wave_target_ids: Vec<_> = affected_waves
        .iter()
        .map(|(target_id, _)| *target_id)
        .collect();
    let wave_ids: Vec<_> = affected_waves
        .iter()
        .map(|(_, wave_id)| wave_id.as_str())
        .collect();

    // Demote first so even a legacy malformed snapshot can switch leaders
    // without transiently violating the one-Crown partial unique index.
    sqlx::query(
        r#"UPDATE "KothApiSnapshotScores" score
              SET is_crown = FALSE
             FROM UNNEST($1::integer[], $2::text[])
                    AS affected(target_id, wave_id)
            WHERE score.target_id = affected.target_id
              AND score.wave_id = affected.wave_id
              AND score.is_crown"#,
    )
    .bind(&wave_target_ids)
    .bind(&wave_ids)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    sqlx::query(
        r#"WITH affected(target_id, wave_id) AS MATERIALIZED (
               SELECT * FROM UNNEST($1::integer[], $2::text[])
           ), completed AS MATERIALIZED (
               SELECT score.target_id, score.wave_id, score.participation_id,
                      score.objective_earned, score.objective_possible
                 FROM "KothApiSnapshotScores" score
                 JOIN affected USING (target_id, wave_id)
                WHERE score.activity_possible > 0
                  AND score.activity_earned = score.activity_possible
                  AND score.objective_earned > 0
                  AND score.objective_possible > 0
           ), leaders AS MATERIALIZED (
               SELECT candidate.*
                 FROM completed candidate
                WHERE NOT EXISTS (
                      SELECT 1
                        FROM completed better
                       WHERE better.target_id = candidate.target_id
                         AND better.wave_id = candidate.wave_id
                         AND better.objective_earned::numeric
                               * candidate.objective_possible::numeric
                             > candidate.objective_earned::numeric
                               * better.objective_possible::numeric
                )
           ), unique_leaders AS (
               SELECT target_id, wave_id,
                      MIN(participation_id) AS participation_id
                 FROM leaders
                GROUP BY target_id, wave_id
               HAVING COUNT(*) = 1
           )
           UPDATE "KothApiSnapshotScores" score
              SET is_crown = TRUE
             FROM unique_leaders leader
            WHERE score.target_id = leader.target_id
              AND score.wave_id = leader.wave_id
              AND score.participation_id = leader.participation_id"#,
    )
    .bind(&wave_target_ids)
    .bind(&wave_ids)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let affected_targets: BTreeSet<_> = target_ids.iter().copied().collect();
    let snapshots: Vec<_> = if fence_capability_change {
        snapshots
    } else {
        snapshots
            .into_iter()
            .filter(|(target_id, _)| affected_targets.contains(target_id))
            .collect()
    };
    if !fence_capability_change && snapshots.len() != target_ids.len() {
        return Err(AppError::internal(
            "Leaderboard snapshot disappeared during capability revocation",
        ));
    }
    bump_snapshot_versions(connection, snapshots).await?;
    Ok(removed.len() as u64)
}

pub(crate) async fn clear_unsettled_scores(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: Option<i32>,
    target_id: Option<i32>,
    participation_ids: &[i32],
) -> AppResult<u64> {
    clear_unsettled_scores_inner(
        connection,
        game_id,
        challenge_id,
        target_id,
        participation_ids,
        false,
    )
    .await
}

/// Rotate an arena identity and fence every matching unsettled parent exactly
/// once, even when that identity had no stored score row yet.
pub(crate) async fn clear_unsettled_scores_for_capability_change(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    participation_ids: &[i32],
) -> AppResult<u64> {
    clear_unsettled_scores_inner(
        connection,
        game_id,
        Some(challenge_id),
        None,
        participation_ids,
        true,
    )
    .await
}

/// Advance the checker stability fence after a capability-generation change,
/// including when the rotated team had not yet produced a score row.
pub(crate) async fn invalidate_unsettled_snapshot_versions(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<u64> {
    let snapshots: Vec<(i32, Vec<u8>)> = sqlx::query_as(
        r#"SELECT target_id, snapshot_hash
             FROM "KothApiSnapshots"
            WHERE game_id = $1 AND challenge_id = $2
            ORDER BY target_id
            FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    bump_snapshot_versions(connection, snapshots).await
}

/// Rebase one accepted observation onto the capability set that produced its
/// opaque context. This closes eligibility gaps even when a status/account/team
/// mutation reaches observation admission before its asynchronous cleanup path.
pub(crate) async fn retain_eligible_unsettled_scores(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    target_id: i32,
    eligible_participation_ids: &[i32],
) -> AppResult<u64> {
    let stale_participation_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT DISTINCT score.participation_id
             FROM "KothApiSnapshotScores" score
             JOIN "KothApiSnapshots" snapshot
               ON snapshot.target_id = score.target_id
            WHERE snapshot.game_id = $1
              AND snapshot.challenge_id = $2
              AND snapshot.target_id = $3
              AND NOT (score.participation_id = ANY($4))
            ORDER BY score.participation_id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(target_id)
    .bind(eligible_participation_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    clear_unsettled_scores(
        connection,
        game_id,
        Some(challenge_id),
        Some(target_id),
        &stale_participation_ids,
    )
    .await
}

/// The official snapshot is the scoring contract. Looking at mutable challenge
/// settings here could accidentally change a running hill from cycle-scoped to
/// event-scoped authentication.
pub(crate) async fn is_api_hill(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1
               FROM "KothOfficialConfigs" config
               JOIN LATERAL jsonb_array_elements(config.hills_snapshot) hill
                 ON (hill->>'challengeId')::integer = $2
                AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
              WHERE config.game_id = $1
           )"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Capture the first live cycle capability for an API hill. Subsequent pristine
/// resets deliberately do nothing, leaving the event token unchanged.
pub(crate) async fn ensure_for_cycle(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    cycle_id: i64,
    reset_attempt: i32,
    target_id: i32,
) -> AppResult<u64> {
    sqlx::query(
        r#"INSERT INTO "KothApiTeamTokens"
               (game_id, challenge_id, participation_id, token)
           SELECT $1, $2, capability.participation_id, capability.token
             FROM "KothTokens" capability
             JOIN "KothOfficialConfigs" config ON config.game_id = $1
             JOIN LATERAL jsonb_array_elements(config.hills_snapshot) hill
               ON (hill->>'challengeId')::integer = $2
              AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
            WHERE capability.cycle_id = $3
              AND capability.challenge_id = $2
              AND capability.reset_attempt = $4
              AND capability.target_id = $5
              AND capability.revoked_at IS NULL
           ON CONFLICT DO NOTHING"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(cycle_id)
    .bind(reset_attempt)
    .bind(target_id)
    .execute(&mut *connection)
    .await
    .map(|result| result.rows_affected())
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Validate one opaque token against its exact event and hill scope. The
/// response exposes an authoritative display name but no local database IDs.
pub(crate) async fn authenticate(
    connection: &mut sqlx::PgConnection,
    token: &str,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Option<AuthenticatedApiTeam>> {
    if !is_well_formed(token) {
        return Ok(None);
    }
    sqlx::query_as::<_, AuthenticatedApiTeam>(
        r#"WITH eligible AS MATERIALIZED (
             SELECT credential.game_id, credential.challenge_id,
                    credential.participation_id, team.name AS team_name
               FROM "KothApiTeamTokens" credential
               JOIN "Games" game
                 ON game.id = credential.game_id
                AND clock_timestamp() >= game.start_time_utc
                AND clock_timestamp() < game.end_time_utc
               JOIN "GameChallenges" challenge
                 ON challenge.game_id = credential.game_id
                AND challenge.id = credential.challenge_id
                AND challenge.is_enabled = TRUE
                AND challenge.review_status = $4
                AND challenge."Type" = $5
               JOIN "Participations" participation
                 ON participation.game_id = credential.game_id
                AND participation.id = credential.participation_id
                AND participation.status = $6
               JOIN "Teams" team
                 ON team.id = participation.team_id
                AND NOT team.deletion_pending
               JOIN "KothOfficialConfigs" config
                 ON config.game_id = credential.game_id
               JOIN LATERAL jsonb_array_elements(config.hills_snapshot) hill
                 ON (hill->>'challengeId')::integer = credential.challenge_id
                AND COALESCE(NULLIF(hill->>'claimSource', ''), 'Marker') = 'Api'
               JOIN LATERAL jsonb_array_elements(config.roster_snapshot) roster(item)
                 ON participation.id = CASE jsonb_typeof(roster.item)
                      WHEN 'number' THEN (roster.item #>> '{}')::integer
                      WHEN 'object' THEN
                        NULLIF(roster.item->>'participationId', '')::integer
                      ELSE NULL
                    END
              WHERE credential.token = $1
                AND credential.game_id = $2
                AND credential.challenge_id = $3
                AND NOT credential.revocation_pending
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
                     WHERE account.id IS NULL OR account.role = $7
                )
              LIMIT 1
           ), touched AS (
             UPDATE "KothApiTeamTokens" credential
                SET last_used_at = clock_timestamp()
               FROM eligible
              WHERE credential.game_id = eligible.game_id
                AND credential.challenge_id = eligible.challenge_id
                AND credential.participation_id = eligible.participation_id
                AND (
                    credential.last_used_at IS NULL
                    OR credential.last_used_at < clock_timestamp() - interval '30 seconds'
                )
             RETURNING credential.participation_id
           )
           SELECT game_id, challenge_id, participation_id, team_name FROM eligible"#,
    )
    .bind(token)
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use sqlx::{Connection, PgConnection};

    use super::{
        authenticate, clear_unsettled_scores, ensure_for_cycle,
        invalidate_unsettled_snapshot_versions, is_well_formed, next_snapshot_version,
        token_hash_hex,
    };

    #[test]
    fn capability_shape_is_bounded_and_unambiguous() {
        assert!(is_well_formed("koth_exampleToken-123456"));
        assert!(is_well_formed("koth_12345678"));
        assert!(!is_well_formed("exampleToken-123456"));
        assert!(!is_well_formed("koth_short"));
        assert!(!is_well_formed("koth_invalid.token"));
        assert!(!is_well_formed(&format!("koth_{}", "a".repeat(129))));
    }

    #[test]
    fn arena_identity_is_the_capability_sha256() {
        assert_eq!(
            token_hash_hex("koth_exampleToken-123456"),
            "015cc9b14ef25b13238c7d2b314019aab355f04e0af6eb0767e5c95616aea6d6"
        );
    }

    #[test]
    fn snapshot_versions_advance_across_byte_carry_and_wrap() {
        let mut carry = [0_u8; 32];
        carry[30] = 7;
        carry[31] = u8::MAX;
        let next = next_snapshot_version(&carry).unwrap();
        assert_eq!(next[30], 8);
        assert_eq!(next[31], 0);
        assert_ne!(next, carry);

        let wrapped = next_snapshot_version(&[u8::MAX; 32]).unwrap();
        assert_eq!(wrapped, [0; 32]);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn api_token_survives_reset_and_rotation_invalidates_the_old_value() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ,
              end_time_utc TIMESTAMPTZ
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER, game_id INTEGER, is_enabled BOOLEAN,
              review_status SMALLINT, "Type" SMALLINT
            );
            CREATE TEMP TABLE "Participations" (
              id INTEGER, game_id INTEGER, team_id INTEGER, status SMALLINT
            );
            CREATE TEMP TABLE "Teams" (
              id INTEGER, name TEXT, captain_id INTEGER,
              deletion_pending BOOLEAN
            );
            CREATE TEMP TABLE "TeamMembers" (team_id INTEGER, user_id INTEGER);
            CREATE TEMP TABLE "AspNetUsers" (id INTEGER, role SMALLINT);
            CREATE TEMP TABLE "KothOfficialConfigs" (
              game_id INTEGER, roster_snapshot JSONB, hills_snapshot JSONB
            );
            CREATE TEMP TABLE "KothTokens" (
              target_id INTEGER, cycle_id BIGINT, challenge_id INTEGER,
              reset_attempt INTEGER, participation_id INTEGER,
              token TEXT, revoked_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothApiTeamTokens" (
              game_id INTEGER, challenge_id INTEGER,
              participation_id INTEGER, token TEXT,
              generation INTEGER DEFAULT 1,
              rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              last_used_at TIMESTAMPTZ,
              revocation_pending BOOLEAN NOT NULL DEFAULT FALSE,
              PRIMARY KEY (game_id, challenge_id, participation_id),
              UNIQUE (token)
            );
            CREATE TEMP TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER,
              challenge_id INTEGER, snapshot_hash BYTEA
            );
            CREATE TEMP TABLE "KothApiSnapshotScores" (
              target_id INTEGER, wave_id TEXT, participation_id INTEGER,
              activity_earned BIGINT, activity_possible BIGINT,
              objective_earned BIGINT, objective_possible BIGINT,
              objective_count SMALLINT, is_crown BOOLEAN,
              PRIMARY KEY (target_id, wave_id, participation_id)
            );
            CREATE UNIQUE INDEX uq_test_koth_api_crown
              ON "KothApiSnapshotScores" (target_id, wave_id)
              WHERE is_crown;
            INSERT INTO "Games" VALUES
              (7, clock_timestamp() - interval '1 minute',
                  clock_timestamp() + interval '1 hour');
            INSERT INTO "GameChallenges" VALUES (9, 7, TRUE, 0, 5);
            INSERT INTO "Participations" VALUES (11, 7, 21, 1);
            INSERT INTO "Teams" VALUES (21, 'Tempo Crew', 101, FALSE);
            INSERT INTO "AspNetUsers" VALUES (101, 1);
            INSERT INTO "KothOfficialConfigs" VALUES
              (7, '[11]', '[{"challengeId":9,"claimSource":"Api"}]');
            INSERT INTO "KothTokens" VALUES
              (3, 41, 9, 0, 11, 'koth_first_reset_token', NULL);
            INSERT INTO "KothApiSnapshots" VALUES
              (3, 7, 9, decode(repeat('11', 32), 'hex')),
              (4, 7, 10, decode(repeat('22', 32), 'hex'));
            INSERT INTO "KothApiSnapshotScores" VALUES
              (3, 'wave-1', 11, 1, 1, 2, 3, 1, TRUE),
              (3, 'wave-1', 12, 1, 1, 1, 2, 1, FALSE),
              (4, 'wave-1', 11, 1, 1, 1, 1, 1, TRUE);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        assert_eq!(
            ensure_for_cycle(&mut connection, 7, 9, 41, 0, 3)
                .await
                .unwrap(),
            1
        );
        sqlx::raw_sql(
            r#"DELETE FROM "KothTokens";
               INSERT INTO "KothTokens" VALUES
                 (3, 42, 9, 0, 11, 'koth_second_reset_token', NULL);"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            ensure_for_cycle(&mut connection, 7, 9, 42, 0, 3)
                .await
                .unwrap(),
            0
        );
        let preserved: String = sqlx::query_scalar(
            r#"SELECT token FROM "KothApiTeamTokens"
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 11"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(preserved, "koth_first_reset_token");
        let identity = authenticate(&mut connection, &preserved, 7, 9)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.game_id, 7);
        assert_eq!(identity.challenge_id, 9);
        assert_eq!(identity.participation_id, 11);
        assert_eq!(identity.team_name, "Tempo Crew");

        let mut rotation = connection.begin().await.unwrap();
        sqlx::query(
            r#"UPDATE "KothApiTeamTokens"
                  SET token = 'koth_rotated_token', generation = generation + 1
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 11"#,
        )
        .execute(&mut *rotation)
        .await
        .unwrap();
        assert!(authenticate(&mut rotation, &preserved, 7, 9)
            .await
            .unwrap()
            .is_none());
        assert!(authenticate(&mut rotation, "koth_rotated_token", 7, 9)
            .await
            .unwrap()
            .is_some());

        assert_eq!(
            clear_unsettled_scores(&mut rotation, 7, Some(9), None, &[11])
                .await
                .unwrap(),
            1
        );
        rotation.commit().await.unwrap();
        let preserved_scores: Vec<(i32, i32)> = sqlx::query_as(
            r#"SELECT target_id, participation_id
                 FROM "KothApiSnapshotScores"
                ORDER BY target_id, participation_id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(preserved_scores, vec![(3, 12), (4, 11)]);

        let before_fence: Vec<u8> = sqlx::query_scalar(
            r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            clear_unsettled_scores(&mut connection, 7, Some(9), None, &[11])
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            invalidate_unsettled_snapshot_versions(&mut connection, 7, 9)
                .await
                .unwrap(),
            1
        );
        let after_fence: Vec<u8> = sqlx::query_scalar(
            r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_ne!(after_fence, before_fence);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn score_redaction_recomputes_exact_crowns_and_versions_atomically() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, snapshot_hash BYTEA NOT NULL
            );
            CREATE TEMP TABLE "KothApiSnapshotScores" (
              target_id INTEGER NOT NULL, wave_id TEXT NOT NULL,
              participation_id INTEGER NOT NULL,
              activity_earned BIGINT NOT NULL,
              activity_possible BIGINT NOT NULL,
              objective_earned BIGINT NOT NULL,
              objective_possible BIGINT NOT NULL,
              objective_count SMALLINT NOT NULL,
              is_crown BOOLEAN NOT NULL,
              PRIMARY KEY (target_id, wave_id, participation_id)
            );
            CREATE UNIQUE INDEX uq_test_koth_api_crown
              ON "KothApiSnapshotScores" (target_id, wave_id)
              WHERE is_crown;
            INSERT INTO "KothApiSnapshots" VALUES
              (3, 7, 9, decode(repeat('11', 32), 'hex')),
              (4, 7, 10, decode(repeat('22', 32), 'hex'));
            INSERT INTO "KothApiSnapshotScores" VALUES
              (3, 'crown-removed', 11, 1, 1, 2, 3, 1, TRUE),
              (3, 'crown-removed', 12, 1, 1, 1, 2, 1, FALSE),
              (3, 'non-crown-removed', 11, 1, 1, 1, 3, 1, FALSE),
              (3, 'non-crown-removed', 12, 1, 1, 2, 3, 1, TRUE),
              (3, 'tie-broken', 11, 1, 1, 2, 3, 1, FALSE),
              (3, 'tie-broken', 12, 1, 1, 4, 6, 1, FALSE),
              (3, 'tie-remains', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'tie-remains', 12, 1, 1, 2, 3, 1, FALSE),
              (3, 'tie-remains', 13, 1, 1, 4, 6, 1, FALSE),
              (3, 'wrong-crown-repaired', 11, 1, 1, 1, 4, 1, FALSE),
              (3, 'wrong-crown-repaired', 12, 1, 1, 1, 2, 1, TRUE),
              (3, 'wrong-crown-repaired', 13, 1, 1, 3, 4, 1, FALSE),
              (3, 'last-row', 11, 1, 1, 1, 1, 1, TRUE),
              (4, 'other-hill', 11, 1, 1, 1, 1, 1, TRUE);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let before: Vec<(i32, Vec<u8>)> = sqlx::query_as(
            r#"SELECT target_id, snapshot_hash
                 FROM "KothApiSnapshots" ORDER BY target_id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        let mut transaction = connection.begin().await.unwrap();
        assert_eq!(
            clear_unsettled_scores(&mut transaction, 7, Some(9), None, &[11])
                .await
                .unwrap(),
            6
        );
        let rows: Vec<(String, i32, bool)> = sqlx::query_as(
            r#"SELECT wave_id, participation_id, is_crown
                 FROM "KothApiSnapshotScores"
                WHERE target_id = 3
                ORDER BY wave_id, participation_id"#,
        )
        .fetch_all(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("crown-removed".to_string(), 12, true),
                ("non-crown-removed".to_string(), 12, true),
                ("tie-broken".to_string(), 12, true),
                ("tie-remains".to_string(), 12, false),
                ("tie-remains".to_string(), 13, false),
                ("wrong-crown-repaired".to_string(), 12, false),
                ("wrong-crown-repaired".to_string(), 13, true),
            ]
        );
        let during: Vec<(i32, Vec<u8>)> = sqlx::query_as(
            r#"SELECT target_id, snapshot_hash
                 FROM "KothApiSnapshots" ORDER BY target_id"#,
        )
        .fetch_all(&mut *transaction)
        .await
        .unwrap();
        assert_ne!(during[0].1, before[0].1);
        assert_eq!(during[1].1, before[1].1);
        transaction.commit().await.unwrap();

        assert_eq!(
            clear_unsettled_scores(&mut connection, 7, None, None, &[11])
                .await
                .unwrap(),
            1
        );
        let remaining_other_hill: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "KothApiSnapshotScores" WHERE target_id = 4"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(remaining_other_hill, 0);
        let after_other_hill: Vec<u8> = sqlx::query_scalar(
            r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 4"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_ne!(after_other_hill, before[1].1);
    }
}
