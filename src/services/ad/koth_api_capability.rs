//! Event-scoped player authentication for Leaderboard/API KotH arenas.

use sha2::{Digest, Sha256};

use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct AuthenticatedApiTeam {
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

/// Remove only the rotating team's unsettled evidence. Deleting the parent
/// snapshot would also erase every other team's current-tick result and make
/// credential rotation an arena-wide denial-of-service primitive.
pub(crate) async fn clear_unsettled_score(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
) -> AppResult<u64> {
    sqlx::query(
        r#"DELETE FROM "KothApiSnapshotScores" score
            USING "KothApiSnapshots" snapshot
            WHERE snapshot.target_id = score.target_id
              AND snapshot.game_id = $1
              AND snapshot.challenge_id = $2
              AND score.participation_id = $3"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(participation_id)
    .execute(&mut *connection)
    .await
    .map(|result| result.rows_affected())
    .map_err(|error| AppError::internal(error.to_string()))
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
           SELECT team_name FROM eligible"#,
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
        authenticate, clear_unsettled_score, ensure_for_cycle, is_well_formed, token_hash_hex,
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
              generation INTEGER DEFAULT 1, last_used_at TIMESTAMPTZ,
              PRIMARY KEY (game_id, challenge_id, participation_id),
              UNIQUE (token)
            );
            CREATE TEMP TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER,
              challenge_id INTEGER
            );
            CREATE TEMP TABLE "KothApiSnapshotScores" (
              target_id INTEGER, participation_id INTEGER,
              PRIMARY KEY (target_id, participation_id)
            );
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
            INSERT INTO "KothApiSnapshots" VALUES (3, 7, 9), (4, 7, 10);
            INSERT INTO "KothApiSnapshotScores" VALUES
              (3, 11), (3, 12), (4, 11);
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
        assert_eq!(identity.team_name, "Tempo Crew");

        sqlx::query(
            r#"UPDATE "KothApiTeamTokens"
                  SET token = 'koth_rotated_token', generation = generation + 1
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 11"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        assert!(authenticate(&mut connection, &preserved, 7, 9)
            .await
            .unwrap()
            .is_none());
        assert!(authenticate(&mut connection, "koth_rotated_token", 7, 9)
            .await
            .unwrap()
            .is_some());

        assert_eq!(
            clear_unsettled_score(&mut connection, 7, 9, 11)
                .await
                .unwrap(),
            1
        );
        let preserved_scores: Vec<(i32, i32)> = sqlx::query_as(
            r#"SELECT target_id, participation_id
                 FROM "KothApiSnapshotScores"
                ORDER BY target_id, participation_id"#,
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(preserved_scores, vec![(3, 12), (4, 11)]);
    }
}
