//! Make logical game/exercise instances unique. Both provisioning paths used a
//! read-then-insert sequence, so concurrent requests could create duplicate rows
//! and orphan backend containers when one pointer overwrote another.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                LOCK TABLE "GameInstances", "ExerciseInstances", "Containers"
                  IN SHARE ROW EXCLUSIVE MODE;

                CREATE TEMP TABLE game_instance_dups ON COMMIT DROP AS
                SELECT id,
                       first_value(id) OVER (
                         PARTITION BY participation_id, challenge_id
                         ORDER BY (container_id IS NOT NULL) DESC,
                                  is_loaded DESC,
                                  last_container_operation DESC,
                                  id
                       ) AS keep_id,
                       first_value(container_id) OVER (
                         PARTITION BY participation_id, challenge_id
                         ORDER BY (container_id IS NOT NULL) DESC,
                                  is_loaded DESC,
                                  last_container_operation DESC,
                                  id
                       ) AS keep_container_id,
                       row_number() OVER (
                         PARTITION BY participation_id, challenge_id
                         ORDER BY (container_id IS NOT NULL) DESC,
                                  is_loaded DESC,
                                  last_container_operation DESC,
                                  id
                       ) AS rn
                FROM "GameInstances";

                UPDATE "GameInstances" keep
                   SET is_loaded = merged.is_loaded,
                       last_container_operation = merged.last_container_operation,
                       flag_id = COALESCE(keep.flag_id, merged.flag_id)
                  FROM (
                    SELECT keep_id,
                           bool_or(is_loaded) AS is_loaded,
                           max(last_container_operation) AS last_container_operation,
                           max(flag_id) AS flag_id
                      FROM "GameInstances" g
                      JOIN game_instance_dups d ON d.id = g.id
                     GROUP BY keep_id
                  ) merged
                 WHERE keep.id = merged.keep_id;

                UPDATE "Containers" c
                   SET game_instance_id = NULL,
                       expect_stop_at = LEAST(expect_stop_at, now())
                  FROM game_instance_dups d
                 WHERE c.game_instance_id = d.id
                   AND c.id IS DISTINCT FROM d.keep_container_id;

                UPDATE "Containers" c
                   SET game_instance_id = d.keep_id
                  FROM game_instance_dups d
                 WHERE d.rn = 1 AND c.id = d.keep_container_id;

                DELETE FROM "GameInstances" g
                 USING game_instance_dups d
                 WHERE d.rn > 1 AND g.id = d.id;

                CREATE UNIQUE INDEX IF NOT EXISTS ux_gameinstances_participation_challenge
                  ON "GameInstances"(participation_id, challenge_id);

                CREATE TEMP TABLE exercise_instance_dups ON COMMIT DROP AS
                SELECT id,
                       first_value(id) OVER (
                         PARTITION BY user_id, exercise_id
                         ORDER BY (container_id IS NOT NULL) DESC,
                                  is_loaded DESC,
                                  is_solved DESC,
                                  last_container_operation DESC,
                                  id
                       ) AS keep_id,
                       first_value(container_id) OVER (
                         PARTITION BY user_id, exercise_id
                         ORDER BY (container_id IS NOT NULL) DESC,
                                  is_loaded DESC,
                                  is_solved DESC,
                                  last_container_operation DESC,
                                  id
                       ) AS keep_container_id,
                       row_number() OVER (
                         PARTITION BY user_id, exercise_id
                         ORDER BY (container_id IS NOT NULL) DESC,
                                  is_loaded DESC,
                                  is_solved DESC,
                                  last_container_operation DESC,
                                  id
                       ) AS rn
                FROM "ExerciseInstances";

                UPDATE "ExerciseInstances" keep
                   SET is_loaded = merged.is_loaded,
                       is_solved = merged.is_solved,
                       last_container_operation = merged.last_container_operation,
                       flag_id = COALESCE(keep.flag_id, merged.flag_id)
                  FROM (
                    SELECT keep_id,
                           bool_or(is_loaded) AS is_loaded,
                           bool_or(is_solved) AS is_solved,
                           max(last_container_operation) AS last_container_operation,
                           max(flag_id) AS flag_id
                      FROM "ExerciseInstances" e
                      JOIN exercise_instance_dups d ON d.id = e.id
                     GROUP BY keep_id
                  ) merged
                 WHERE keep.id = merged.keep_id;

                UPDATE "Containers" c
                   SET exercise_instance_id = NULL,
                       expect_stop_at = LEAST(expect_stop_at, now())
                  FROM exercise_instance_dups d
                 WHERE c.exercise_instance_id = d.id
                   AND c.id IS DISTINCT FROM d.keep_container_id;

                UPDATE "Containers" c
                   SET exercise_instance_id = d.keep_id
                  FROM exercise_instance_dups d
                 WHERE d.rn = 1 AND c.id = d.keep_container_id;

                DELETE FROM "ExerciseInstances" e
                 USING exercise_instance_dups d
                 WHERE d.rn > 1 AND e.id = d.id;

                CREATE UNIQUE INDEX IF NOT EXISTS ux_exerciseinstances_user_exercise
                  ON "ExerciseInstances"(user_id, exercise_id);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP INDEX IF EXISTS ux_gameinstances_participation_challenge;
                DROP INDEX IF EXISTS ux_exerciseinstances_user_exercise;
                "#,
            )
            .await?;
        Ok(())
    }
}
