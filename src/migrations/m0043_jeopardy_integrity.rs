//! Repair legacy jeopardy bookkeeping and enforce its scoring invariants.

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
                LOCK TABLE "Games", "Teams", "AspNetUsers", "Divisions",
                           "DivisionChallengeConfigs", "Participations",
                           "GameChallenges", "Submissions", "FirstSolves", "CheatInfo"
                  IN SHARE ROW EXCLUSIVE MODE;

                -- A stale division reference used to fail open to ALL permissions.
                -- Quarantine malformed accepted rows before clearing the bad pointer.
                UPDATE "Participations" participation
                   SET status = 2, division_id = NULL
                 WHERE participation.division_id IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1 FROM "Divisions" division
                          WHERE division.id = participation.division_id
                            AND division.game_id = participation.game_id
                   );

                DELETE FROM "DivisionChallengeConfigs" permission
                 WHERE NOT EXISTS (
                       SELECT 1 FROM "Divisions" division
                        WHERE division.id = permission.division_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "GameChallenges" challenge
                        WHERE challenge.id = permission.challenge_id
                 )
                    OR NOT EXISTS (
                       SELECT 1
                         FROM "Divisions" division
                         JOIN "GameChallenges" challenge
                           ON challenge.game_id = division.game_id
                        WHERE division.id = permission.division_id
                          AND challenge.id = permission.challenge_id
                 );

                UPDATE "Submissions" submission
                   SET user_id = NULL
                 WHERE submission.user_id IS NOT NULL
                   AND NOT EXISTS (
                         SELECT 1 FROM "AspNetUsers" account
                          WHERE account.id = submission.user_id
                   );

                DELETE FROM "Submissions" submission
                 WHERE NOT EXISTS (
                       SELECT 1 FROM "Games" game
                        WHERE game.id = submission.game_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "Teams" team
                        WHERE team.id = submission.team_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "GameChallenges" challenge
                        WHERE challenge.id = submission.challenge_id
                          AND challenge.game_id = submission.game_id
                 )
                    OR NOT EXISTS (
                       SELECT 1 FROM "Participations" participation
                        WHERE participation.id = submission.participation_id
                          AND participation.game_id = submission.game_id
                          AND participation.team_id = submission.team_id
                 );

                DELETE FROM "CheatInfo" cheat
                 WHERE NOT EXISTS (
                       SELECT 1 FROM "Submissions" submission
                        WHERE submission.id = cheat.submission_id
                 );

                -- Remove dangling/mismatched first-solve projections, then rebuild
                -- every pair from its earliest accepted canonical submission.
                DELETE FROM "FirstSolves" first_solve
                 WHERE NOT EXISTS (
                       SELECT 1
                         FROM "Participations" participation
                         JOIN "GameChallenges" challenge
                           ON challenge.id = first_solve.challenge_id
                          AND challenge.game_id = participation.game_id
                         JOIN "Submissions" submission
                           ON submission.id = first_solve.submission_id
                          AND submission.participation_id = first_solve.participation_id
                          AND submission.challenge_id = first_solve.challenge_id
                          AND submission.game_id = participation.game_id
                          AND submission.status = 1
                        WHERE participation.id = first_solve.participation_id
                 );

                INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
                SELECT DISTINCT ON (submission.participation_id, submission.challenge_id)
                       submission.participation_id,
                       submission.challenge_id,
                       submission.id
                  FROM "Submissions" submission
                  JOIN "Participations" participation
                    ON participation.id = submission.participation_id
                   AND participation.game_id = submission.game_id
                  JOIN "GameChallenges" challenge
                    ON challenge.id = submission.challenge_id
                   AND challenge.game_id = submission.game_id
                 WHERE submission.status = 1
                 ORDER BY submission.participation_id, submission.challenge_id,
                          submission.submit_time_utc, submission.id
                ON CONFLICT (participation_id, challenge_id) DO UPDATE
                      SET submission_id = EXCLUDED.submission_id;

                -- Normalize pre-constraint scoring metadata conservatively.
                UPDATE "GameChallenges"
                   SET original_score = GREATEST(original_score, 0),
                       submission_limit = GREATEST(submission_limit, 0),
                       accepted_count = GREATEST(accepted_count, 0),
                       submission_count = GREATEST(submission_count, 0),
                       min_score_rate = CASE
                         WHEN min_score_rate >= 0 AND min_score_rate <= 1
                         THEN min_score_rate ELSE 0.25 END,
                       difficulty = CASE
                         WHEN difficulty > 0
                          AND difficulty < 'Infinity'::double precision
                         THEN difficulty ELSE 5.0 END;

                CREATE UNIQUE INDEX IF NOT EXISTS ux_divisions_game_id
                  ON "Divisions"(game_id, id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_submissions_id_part_challenge
                  ON "Submissions"(id, participation_id, challenge_id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_participations_game_team_id
                  ON "Participations"(game_id, team_id, id);

                DO $$
                BEGIN
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_participations_division_game' AND conrelid = '"Participations"'::regclass) THEN
                    ALTER TABLE "Participations" ADD CONSTRAINT fk_participations_division_game
                      FOREIGN KEY (game_id, division_id)
                      REFERENCES "Divisions"(game_id, id) ON DELETE RESTRICT;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_divisionconfigs_division' AND conrelid = '"DivisionChallengeConfigs"'::regclass) THEN
                    ALTER TABLE "DivisionChallengeConfigs" ADD CONSTRAINT fk_divisionconfigs_division
                      FOREIGN KEY (division_id) REFERENCES "Divisions"(id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_divisionconfigs_challenge' AND conrelid = '"DivisionChallengeConfigs"'::regclass) THEN
                    ALTER TABLE "DivisionChallengeConfigs" ADD CONSTRAINT fk_divisionconfigs_challenge
                      FOREIGN KEY (challenge_id) REFERENCES "GameChallenges"(id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_submissions_game' AND conrelid = '"Submissions"'::regclass) THEN
                    ALTER TABLE "Submissions" ADD CONSTRAINT fk_submissions_game
                      FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_submissions_challenge_game' AND conrelid = '"Submissions"'::regclass) THEN
                    ALTER TABLE "Submissions" ADD CONSTRAINT fk_submissions_challenge_game
                      FOREIGN KEY (game_id, challenge_id) REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_submissions_participation_team_game' AND conrelid = '"Submissions"'::regclass) THEN
                    ALTER TABLE "Submissions" ADD CONSTRAINT fk_submissions_participation_team_game
                      FOREIGN KEY (game_id, team_id, participation_id)
                      REFERENCES "Participations"(game_id, team_id, id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_submissions_team' AND conrelid = '"Submissions"'::regclass) THEN
                    ALTER TABLE "Submissions" ADD CONSTRAINT fk_submissions_team
                      FOREIGN KEY (team_id) REFERENCES "Teams"(id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_submissions_user' AND conrelid = '"Submissions"'::regclass) THEN
                    ALTER TABLE "Submissions" ADD CONSTRAINT fk_submissions_user
                      FOREIGN KEY (user_id) REFERENCES "AspNetUsers"(id) ON DELETE SET NULL;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_firstsolves_participation' AND conrelid = '"FirstSolves"'::regclass) THEN
                    ALTER TABLE "FirstSolves" ADD CONSTRAINT fk_firstsolves_participation
                      FOREIGN KEY (participation_id) REFERENCES "Participations"(id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_firstsolves_challenge' AND conrelid = '"FirstSolves"'::regclass) THEN
                    ALTER TABLE "FirstSolves" ADD CONSTRAINT fk_firstsolves_challenge
                      FOREIGN KEY (challenge_id) REFERENCES "GameChallenges"(id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_firstsolves_submission_pair' AND conrelid = '"FirstSolves"'::regclass) THEN
                    ALTER TABLE "FirstSolves" ADD CONSTRAINT fk_firstsolves_submission_pair
                      FOREIGN KEY (submission_id, participation_id, challenge_id)
                      REFERENCES "Submissions"(id, participation_id, challenge_id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_cheatinfo_submission' AND conrelid = '"CheatInfo"'::regclass) THEN
                    ALTER TABLE "CheatInfo" ADD CONSTRAINT fk_cheatinfo_submission
                      FOREIGN KEY (submission_id) REFERENCES "Submissions"(id) ON DELETE CASCADE;
                  END IF;
                  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_gamechallenges_jeopardy_scoring' AND conrelid = '"GameChallenges"'::regclass) THEN
                    ALTER TABLE "GameChallenges" ADD CONSTRAINT ck_gamechallenges_jeopardy_scoring
                      CHECK (original_score >= 0
                         AND submission_limit >= 0
                         AND accepted_count >= 0
                         AND submission_count >= 0
                         AND min_score_rate >= 0 AND min_score_rate <= 1
                         AND difficulty > 0
                         AND difficulty < 'Infinity'::double precision);
                  END IF;
                END $$;
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
                ALTER TABLE "GameChallenges"
                  DROP CONSTRAINT IF EXISTS ck_gamechallenges_jeopardy_scoring;
                ALTER TABLE "FirstSolves"
                  DROP CONSTRAINT IF EXISTS fk_firstsolves_submission_pair,
                  DROP CONSTRAINT IF EXISTS fk_firstsolves_challenge,
                  DROP CONSTRAINT IF EXISTS fk_firstsolves_participation;
                ALTER TABLE "CheatInfo"
                  DROP CONSTRAINT IF EXISTS fk_cheatinfo_submission;
                ALTER TABLE "DivisionChallengeConfigs"
                  DROP CONSTRAINT IF EXISTS fk_divisionconfigs_challenge,
                  DROP CONSTRAINT IF EXISTS fk_divisionconfigs_division;
                ALTER TABLE "Participations"
                  DROP CONSTRAINT IF EXISTS fk_participations_division_game;
                ALTER TABLE "Submissions"
                  DROP CONSTRAINT IF EXISTS fk_submissions_user,
                  DROP CONSTRAINT IF EXISTS fk_submissions_team,
                  DROP CONSTRAINT IF EXISTS fk_submissions_participation_team_game,
                  DROP CONSTRAINT IF EXISTS fk_submissions_challenge_game,
                  DROP CONSTRAINT IF EXISTS fk_submissions_game;
                DROP INDEX IF EXISTS ux_participations_game_team_id;
                DROP INDEX IF EXISTS ux_submissions_id_part_challenge;
                DROP INDEX IF EXISTS ux_divisions_game_id;
                "#,
            )
            .await?;
        Ok(())
    }
}
