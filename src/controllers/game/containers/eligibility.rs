use sea_orm::EntityTrait;

use crate::app_state::SharedState;
use crate::models::data::{game_challenge, game_challenge::Entity as GameChallenge};
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType, GamePermission,
    ParticipationStatus, Role,
};
use crate::utils::error::{AppError, AppResult};

use super::uses_shared_container;

#[derive(Clone, Copy)]
pub(super) enum ContainerRequestMode {
    PerTeam,
    Shared,
}

/// Re-check every mutable authorization input while the matching lifecycle lock is
/// held. The normal play-context helpers intentionally use short-lived caches; those
/// caches are unsuitable for a create/delete exclusion boundary because an operator
/// can reject a participation, disable a challenge, or change its container mode while
/// a request waits for the lock or for the backend runtime.
pub(super) async fn player_container_request_is_eligible(
    st: &SharedState,
    user_id: uuid::Uuid,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    mode: ContainerRequestMode,
) -> AppResult<bool> {
    player_container_request_is_eligible_on(
        st.pg(),
        user_id,
        game_id,
        participation_id,
        challenge_id,
        mode,
    )
    .await
}

async fn player_container_request_is_eligible_on(
    pool: &sqlx::PgPool,
    user_id: uuid::Uuid,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    mode: ContainerRequestMode,
) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1
                 FROM "Participations" participation
                 JOIN "UserParticipations" link
                   ON link.participation_id = participation.id
                  AND link.game_id = participation.game_id
                  AND link.team_id = participation.team_id
                 JOIN "AspNetUsers" account ON account.id = link.user_id
                 JOIN "Games" game ON game.id = participation.game_id
                 JOIN "GameChallenges" challenge
                   ON challenge.game_id = game.id
                  AND challenge.id = $5
            LEFT JOIN "Divisions" division
                   ON division.id = participation.division_id
                  AND division.game_id = game.id
            LEFT JOIN "DivisionChallengeConfigs" permission
                   ON permission.division_id = participation.division_id
                  AND permission.challenge_id = challenge.id
                WHERE link.user_id = $1
                  AND game.id = $2
                  AND participation.id = $3
                  AND participation.status = $6
                  AND account.role <> $7
                  AND game.deletion_pending = FALSE
                  AND game.start_time_utc <= CURRENT_TIMESTAMP
                  AND (game.practice_mode OR game.end_time_utc >= CURRENT_TIMESTAMP)
                  AND challenge.is_enabled
                  AND challenge.deletion_pending = FALSE
                  AND challenge.review_status = $8
                  AND (challenge.workload_spec IS NOT NULL OR (
                       challenge.build_status = $15
                       AND NULLIF(BTRIM(challenge.build_image_digest), '') IS NOT NULL))
                  AND (
                        participation.division_id IS NULL
                        OR (COALESCE(permission.permissions, division.default_permissions, $9) & $10) = $10
                  )
                  AND (
                       ($4 AND
                            game.end_time_utc >= CURRENT_TIMESTAMP
                        AND challenge."Type" = $11
                        AND challenge.enable_shared_container
                        AND (challenge.workload_spec IS NOT NULL OR (
                             COALESCE(challenge.container_image, '') <> ''
                             AND challenge.expose_port IS NOT NULL)))
                       OR
                       (NOT $4 AND (
                            (challenge."Type" IN ($11, $12)
                             AND NOT (
                                  challenge."Type" = $11
                              AND challenge.enable_shared_container
                              AND (challenge.workload_spec IS NOT NULL OR (
                                   COALESCE(challenge.container_image, '') <> ''
                                   AND challenge.expose_port IS NOT NULL))))
                            OR
                            (challenge."Type" IN ($13, $14)
                             AND game.practice_mode
                             AND game.end_time_utc < CURRENT_TIMESTAMP
                             AND COALESCE(challenge.container_image, '') <> ''
                             AND challenge.expose_port IS NOT NULL)
                       ))
                  )
           )"#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(matches!(mode, ContainerRequestMode::Shared))
    .bind(challenge_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(GamePermission::ALL)
    .bind(GamePermission::VIEW_CHALLENGE)
    .bind(ChallengeType::StaticContainer as i16)
    .bind(ChallengeType::DynamicContainer as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(ChallengeBuildStatus::Success as i16)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn is_shared_container_mode(challenge: &game_challenge::Model) -> bool {
    uses_shared_container(challenge) || challenge.challenge_type == ChallengeType::KingOfTheHill
}

pub(super) async fn load_eligible_shared_challenge(
    st: &SharedState,
    challenge_id: i32,
) -> AppResult<game_challenge::Model> {
    // The provisioning caller needs the complete enum-rich entity to build its
    // ContainerSpec; duplicating that hydration in a large sqlx tuple is more brittle.
    let challenge = GameChallenge::find_by_id(challenge_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    let game_is_live = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1
                 FROM "Games" game
                 JOIN "GameChallenges" candidate
                   ON candidate.game_id = game.id AND candidate.id = $2
                WHERE game.id = $1
                  AND game.end_time_utc >= CURRENT_TIMESTAMP
                  AND game.deletion_pending = FALSE
                  AND candidate.deletion_pending = FALSE
           )"#,
    )
    .bind(challenge.game_id)
    .bind(challenge.id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !game_is_live
        || !challenge.is_enabled
        || challenge.review_status != ChallengeReviewStatus::Active
        || !is_shared_container_mode(&challenge)
    {
        return Err(AppError::bad_request(
            "Shared container provisioning is no longer allowed",
        ));
    }
    Ok(challenge)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn deletion_pending_game_or_challenge_is_never_container_eligible() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("container_pending_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, role SMALLINT NOT NULL);
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL,
              practice_mode BOOLEAN NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, division_id INTEGER,
              status SMALLINT NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              participation_id INTEGER NOT NULL, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, user_id UUID NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              is_enabled BOOLEAN NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              review_status SMALLINT NOT NULL, workload_spec JSONB,
              build_status SMALLINT NOT NULL, build_image_digest TEXT,
              "Type" SMALLINT NOT NULL, enable_shared_container BOOLEAN NOT NULL,
              container_image TEXT, expose_port INTEGER
            );
            CREATE TABLE "Divisions" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              default_permissions INTEGER NOT NULL
            );
            CREATE TABLE "DivisionChallengeConfigs" (
              division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              permissions INTEGER NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let user_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, 1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            INSERT INTO "Games" VALUES
              (1, clock_timestamp() - interval '1 hour',
               clock_timestamp() + interval '1 hour', FALSE, FALSE);
            INSERT INTO "Participations" VALUES (2, 1, 3, NULL, 1);
            INSERT INTO "GameChallenges" VALUES
              (4, 1, TRUE, FALSE, 0, NULL, 1,
               'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
               1, FALSE, 'ignored:latest', 8080);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "UserParticipations" VALUES (2, 1, 3, $1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();

        assert!(player_container_request_is_eligible_on(
            &pool,
            user_id,
            1,
            2,
            4,
            ContainerRequestMode::PerTeam,
        )
        .await
        .unwrap());
        sqlx::query(r#"UPDATE "GameChallenges" SET deletion_pending = TRUE WHERE id = 4"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!player_container_request_is_eligible_on(
            &pool,
            user_id,
            1,
            2,
            4,
            ContainerRequestMode::PerTeam,
        )
        .await
        .unwrap());
        sqlx::raw_sql(
            r#"UPDATE "GameChallenges" SET deletion_pending = FALSE WHERE id = 4;
               UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1;"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(!player_container_request_is_eligible_on(
            &pool,
            user_id,
            1,
            2,
            4,
            ContainerRequestMode::PerTeam,
        )
        .await
        .unwrap());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
