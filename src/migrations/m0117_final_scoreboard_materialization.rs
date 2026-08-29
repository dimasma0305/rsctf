//! Durable, replica-safe closeout state for immutable final scoreboards.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "FinalScoreboardMaterializations" (
    game_id              INTEGER PRIMARY KEY,
    game_end_time_utc    TIMESTAMPTZ NOT NULL,
    available_at_utc     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    invalidated_at_utc   TIMESTAMPTZ NULL,
    completed_at_utc     TIMESTAMPTZ NULL,
    dead_at_utc          TIMESTAMPTZ NULL,
    lease_token          UUID NULL,
    lease_expires_at_utc TIMESTAMPTZ NULL,
    attempts             INTEGER NOT NULL DEFAULT 0,
    last_error           VARCHAR(256) NULL,
    updated_at_utc       TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_final_scoreboard_materialization_game
        FOREIGN KEY (game_id) REFERENCES "Games" (id) ON DELETE CASCADE,
    CONSTRAINT ck_final_scoreboard_materialization_attempts
        CHECK (attempts >= 0 AND attempts <= 16),
    CONSTRAINT ck_final_scoreboard_materialization_lease_pair
        CHECK ((lease_token IS NULL) = (lease_expires_at_utc IS NULL)),
    CONSTRAINT ck_final_scoreboard_materialization_terminal
        CHECK (NOT (completed_at_utc IS NOT NULL AND dead_at_utc IS NOT NULL)),
    CONSTRAINT ck_final_scoreboard_materialization_completed
        CHECK (completed_at_utc IS NULL OR invalidated_at_utc IS NOT NULL),
    CONSTRAINT ck_final_scoreboard_materialization_terminal_lease
        CHECK ((completed_at_utc IS NULL AND dead_at_utc IS NULL)
               OR (lease_token IS NULL AND lease_expires_at_utc IS NULL))
);

CREATE INDEX IF NOT EXISTS ix_final_scoreboard_materialization_pending
    ON "FinalScoreboardMaterializations" (available_at_utc, game_id)
    WHERE completed_at_utc IS NULL AND dead_at_utc IS NULL;

-- Every score-affecting post-end mutation records durable repair intent in the
-- same PostgreSQL transaction as the mutation. Cache eviction remains a fast
-- post-commit optimization; a crash in that window cannot strand a 24h render.
CREATE OR REPLACE FUNCTION rsctf_request_final_scoreboard_repair(p_game_id INTEGER)
RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO "FinalScoreboardMaterializations"
           (game_id, game_end_time_utc, available_at_utc)
    SELECT game.id, game.end_time_utc, clock_timestamp()
      FROM "Games" game
     WHERE game.id = p_game_id
       AND game.end_time_utc <= clock_timestamp()
       AND NOT game.practice_mode
    ON CONFLICT (game_id) DO UPDATE SET
           game_end_time_utc = EXCLUDED.game_end_time_utc,
           available_at_utc = EXCLUDED.available_at_utc,
           invalidated_at_utc = NULL,
           completed_at_utc = NULL,
           dead_at_utc = NULL,
           lease_token = CASE
               WHEN "FinalScoreboardMaterializations".lease_expires_at_utc
                    > EXCLUDED.available_at_utc
               THEN "FinalScoreboardMaterializations".lease_token
               ELSE NULL
           END,
           lease_expires_at_utc = CASE
               WHEN "FinalScoreboardMaterializations".lease_expires_at_utc
                    > EXCLUDED.available_at_utc
               THEN "FinalScoreboardMaterializations".lease_expires_at_utc
               ELSE NULL
           END,
           attempts = 0,
           last_error = NULL,
           updated_at_utc = EXCLUDED.available_at_utc;
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_scoreboard_game_row_repair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    old_game_id INTEGER;
    new_game_id INTEGER;
BEGIN
    IF TG_OP <> 'INSERT' THEN old_game_id := OLD.game_id; END IF;
    IF TG_OP <> 'DELETE' THEN new_game_id := NEW.game_id; END IF;
    IF old_game_id IS NOT NULL THEN
        PERFORM rsctf_request_final_scoreboard_repair(old_game_id);
    END IF;
    IF new_game_id IS NOT NULL AND new_game_id IS DISTINCT FROM old_game_id THEN
        PERFORM rsctf_request_final_scoreboard_repair(new_game_id);
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_scoreboard_division_config_repair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    old_game_id INTEGER;
    new_game_id INTEGER;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        SELECT game_id INTO old_game_id FROM "Divisions" WHERE id = OLD.division_id;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        SELECT game_id INTO new_game_id FROM "Divisions" WHERE id = NEW.division_id;
    END IF;
    IF old_game_id IS NOT NULL THEN
        PERFORM rsctf_request_final_scoreboard_repair(old_game_id);
    END IF;
    IF new_game_id IS NOT NULL AND new_game_id IS DISTINCT FROM old_game_id THEN
        PERFORM rsctf_request_final_scoreboard_repair(new_game_id);
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_scoreboard_first_solve_repair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    old_game_id INTEGER;
    new_game_id INTEGER;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        SELECT game_id INTO old_game_id FROM "Submissions" WHERE id = OLD.submission_id;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        SELECT game_id INTO new_game_id FROM "Submissions" WHERE id = NEW.submission_id;
    END IF;
    IF old_game_id IS NOT NULL THEN
        PERFORM rsctf_request_final_scoreboard_repair(old_game_id);
    END IF;
    IF new_game_id IS NOT NULL AND new_game_id IS DISTINCT FROM old_game_id THEN
        PERFORM rsctf_request_final_scoreboard_repair(new_game_id);
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_scoreboard_team_repair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    affected_game_id INTEGER;
BEGIN
    FOR affected_game_id IN
        SELECT DISTINCT game_id FROM "Participations" WHERE team_id = NEW.id
    LOOP
        PERFORM rsctf_request_final_scoreboard_repair(affected_game_id);
    END LOOP;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_scoreboard_account_repair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    affected_game_id INTEGER;
BEGIN
    FOR affected_game_id IN
        SELECT DISTINCT submission.game_id
          FROM "Submissions" submission
         WHERE submission.user_id = NEW.id
           AND submission.status = 1
    LOOP
        PERFORM rsctf_request_final_scoreboard_repair(affected_game_id);
    END LOOP;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION rsctf_scoreboard_game_repair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    PERFORM rsctf_request_final_scoreboard_repair(NEW.id);
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS trg_game_challenges_scoreboard_repair ON "GameChallenges";
CREATE TRIGGER trg_game_challenges_scoreboard_repair
AFTER INSERT OR DELETE OR UPDATE OF game_id, title, category, "Type",
                is_enabled, deadline_utc, accepted_count, review_status,
                disable_blood_bonus, original_score, min_score_rate,
                difficulty, score_curve, ad_scoring_weight
ON "GameChallenges"
FOR EACH ROW EXECUTE FUNCTION rsctf_scoreboard_game_row_repair();
DROP TRIGGER IF EXISTS trg_participations_scoreboard_repair ON "Participations";
CREATE TRIGGER trg_participations_scoreboard_repair
AFTER INSERT OR DELETE OR UPDATE OF game_id, team_id, division_id, status
ON "Participations"
FOR EACH ROW EXECUTE FUNCTION rsctf_scoreboard_game_row_repair();
DROP TRIGGER IF EXISTS trg_divisions_scoreboard_repair ON "Divisions";
CREATE TRIGGER trg_divisions_scoreboard_repair
AFTER INSERT OR UPDATE OR DELETE ON "Divisions"
FOR EACH ROW EXECUTE FUNCTION rsctf_scoreboard_game_row_repair();
DROP TRIGGER IF EXISTS trg_submissions_scoreboard_repair ON "Submissions";
CREATE TRIGGER trg_submissions_scoreboard_repair
AFTER INSERT ON "Submissions"
FOR EACH ROW WHEN (NEW.status = 1)
EXECUTE FUNCTION rsctf_scoreboard_game_row_repair();
DROP TRIGGER IF EXISTS trg_division_configs_scoreboard_repair ON "DivisionChallengeConfigs";
CREATE TRIGGER trg_division_configs_scoreboard_repair
AFTER INSERT OR UPDATE OR DELETE ON "DivisionChallengeConfigs"
FOR EACH ROW EXECUTE FUNCTION rsctf_scoreboard_division_config_repair();
DROP TRIGGER IF EXISTS trg_first_solves_scoreboard_repair ON "FirstSolves";
CREATE TRIGGER trg_first_solves_scoreboard_repair
AFTER INSERT OR UPDATE OR DELETE ON "FirstSolves"
FOR EACH ROW EXECUTE FUNCTION rsctf_scoreboard_first_solve_repair();
DROP TRIGGER IF EXISTS trg_teams_scoreboard_repair ON "Teams";
CREATE TRIGGER trg_teams_scoreboard_repair
AFTER UPDATE OF name, avatar_hash ON "Teams"
FOR EACH ROW EXECUTE FUNCTION rsctf_scoreboard_team_repair();
DROP TRIGGER IF EXISTS trg_accounts_scoreboard_repair ON "AspNetUsers";
CREATE TRIGGER trg_accounts_scoreboard_repair
AFTER UPDATE OF user_name ON "AspNetUsers"
FOR EACH ROW
WHEN (NEW.user_name IS DISTINCT FROM OLD.user_name)
EXECUTE FUNCTION rsctf_scoreboard_account_repair();
DROP TRIGGER IF EXISTS trg_games_scoreboard_repair ON "Games";
CREATE TRIGGER trg_games_scoreboard_repair
AFTER UPDATE OF hidden, practice_mode, start_time_utc, end_time_utc,
                freeze_time_utc, blood_bonus_value, ad_epoch_ticks,
                ad_scoring_start_round, ad_flag_lifetime_ticks, ad_tick_seconds,
                koth_epoch_ticks, koth_cycle_ticks, koth_champion_cooldown_ticks,
                koth_claim_confirmation_ticks
ON "Games" FOR EACH ROW EXECUTE FUNCTION rsctf_scoreboard_game_repair();

CREATE OR REPLACE FUNCTION rsctf_enqueue_final_scoreboard_materialization()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.practice_mode THEN
        DELETE FROM "FinalScoreboardMaterializations" WHERE game_id = NEW.id;
        RETURN NEW;
    END IF;
    INSERT INTO "FinalScoreboardMaterializations"
           (game_id, game_end_time_utc, available_at_utc)
    VALUES (NEW.id, NEW.end_time_utc, NEW.end_time_utc)
    ON CONFLICT (game_id) DO UPDATE SET
           game_end_time_utc = EXCLUDED.game_end_time_utc,
           available_at_utc = EXCLUDED.available_at_utc,
           invalidated_at_utc = NULL,
           completed_at_utc = NULL,
           dead_at_utc = NULL,
           lease_token = NULL,
           lease_expires_at_utc = NULL,
           attempts = 0,
           last_error = NULL,
           updated_at_utc = clock_timestamp()
     WHERE "FinalScoreboardMaterializations".game_end_time_utc
           IS DISTINCT FROM EXCLUDED.game_end_time_utc;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS trg_games_final_scoreboard_materialization ON "Games";
CREATE TRIGGER trg_games_final_scoreboard_materialization
AFTER INSERT OR UPDATE OF end_time_utc, practice_mode ON "Games"
FOR EACH ROW EXECUTE FUNCTION rsctf_enqueue_final_scoreboard_materialization();

-- Existing non-practice events enter the same bounded queue. Future rows wait
-- on `available_at_utc`; no recurring scan of historical Games is necessary.
-- This also avoids silently
-- declaring a board final when an older deployment crashed before sealing its
-- last A&D/KotH evidence; the worker processes only a small batch per tick.
INSERT INTO "FinalScoreboardMaterializations"
       (game_id, game_end_time_utc, available_at_utc)
SELECT id, end_time_utc, end_time_utc
  FROM "Games"
 WHERE NOT practice_mode
ON CONFLICT (game_id) DO NOTHING;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn closeout_queue_is_idempotent_bounded_and_replica_safe() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(UP_SQL.contains("REFERENCES \"Games\" (id) ON DELETE CASCADE"));
        assert!(UP_SQL.contains("attempts >= 0 AND attempts <= 16"));
        assert!(UP_SQL.contains("lease_token IS NULL") && UP_SQL.contains("lease_expires_at_utc"));
        assert!(UP_SQL.contains("completed_at_utc IS NULL AND dead_at_utc IS NULL"));
        assert!(UP_SQL.contains("CREATE TRIGGER trg_games_final_scoreboard_materialization"));
        assert!(UP_SQL.contains("rsctf_request_final_scoreboard_repair"));
        assert!(UP_SQL.contains("trg_game_challenges_scoreboard_repair"));
        assert!(UP_SQL.contains("trg_participations_scoreboard_repair"));
        assert!(UP_SQL.contains("UPDATE OF game_id, title, category, \"Type\""));
        assert!(UP_SQL.contains("UPDATE OF game_id, team_id, division_id, status"));
        assert!(UP_SQL.contains("trg_submissions_scoreboard_repair"));
        assert!(UP_SQL.contains("AFTER INSERT ON \"Submissions\""));
        assert!(UP_SQL.contains("FOR EACH ROW WHEN (NEW.status = 1)"));
        assert!(UP_SQL.contains("trg_first_solves_scoreboard_repair"));
        assert!(UP_SQL.contains("CREATE TRIGGER trg_accounts_scoreboard_repair"));
        assert!(UP_SQL.contains("AFTER UPDATE OF user_name ON \"AspNetUsers\""));
        assert!(UP_SQL.contains("submission.user_id = NEW.id"));
        assert!(UP_SQL.contains("IF NEW.practice_mode"));
        assert!(UP_SQL.contains("VALUES (NEW.id, NEW.end_time_utc, NEW.end_time_utc)"));
        assert!(UP_SQL.contains("ON CONFLICT (game_id) DO NOTHING"));
        assert!(!UP_SQL.contains("scoreboard_json"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_seeds_schedules_and_transactionally_repairs_mutations() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0110_{}", uuid::Uuid::new_v4().simple());
        assert!(schema
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (
                 id INTEGER PRIMARY KEY,
                 end_time_utc TIMESTAMPTZ NOT NULL,
                 practice_mode BOOLEAN NOT NULL DEFAULT FALSE,
                 hidden BOOLEAN NOT NULL DEFAULT FALSE,
                 start_time_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 freeze_time_utc TIMESTAMPTZ,
                 blood_bonus_value BIGINT NOT NULL DEFAULT 0,
                 ad_epoch_ticks INTEGER NOT NULL DEFAULT 8,
                 ad_scoring_start_round INTEGER,
                 ad_flag_lifetime_ticks INTEGER,
                 ad_tick_seconds INTEGER,
                 koth_epoch_ticks INTEGER NOT NULL DEFAULT 12,
                 koth_cycle_ticks INTEGER NOT NULL DEFAULT 3,
                 koth_champion_cooldown_ticks INTEGER NOT NULL DEFAULT 1,
                 koth_claim_confirmation_ticks INTEGER NOT NULL DEFAULT 2
               );
               CREATE TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                 title TEXT NOT NULL DEFAULT '', category SMALLINT NOT NULL DEFAULT 0,
                 "Type" SMALLINT NOT NULL DEFAULT 0,
                 is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
                 deadline_utc TIMESTAMPTZ, accepted_count INTEGER NOT NULL DEFAULT 0,
                 review_status SMALLINT NOT NULL DEFAULT 0,
                 disable_blood_bonus BOOLEAN NOT NULL DEFAULT FALSE,
                 original_score INTEGER NOT NULL DEFAULT 1000,
                 min_score_rate DOUBLE PRECISION NOT NULL DEFAULT 0.25,
                 difficulty DOUBLE PRECISION NOT NULL DEFAULT 5.0,
                 score_curve SMALLINT NOT NULL DEFAULT 0,
                 ad_scoring_weight DOUBLE PRECISION NOT NULL DEFAULT 1.0,
                 build_status SMALLINT NOT NULL DEFAULT 0
               );
               CREATE TABLE "Participations" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                 team_id INTEGER NOT NULL, division_id INTEGER,
                 status SMALLINT NOT NULL DEFAULT 0,
                 suspicion_score INTEGER NOT NULL DEFAULT 0
               );
               CREATE TABLE "Divisions" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL
               );
               CREATE TABLE "DivisionChallengeConfigs" (
                 division_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
               );
               CREATE TABLE "Submissions" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                 status SMALLINT NOT NULL DEFAULT 0,
                 user_id UUID
               );
               CREATE TABLE "FirstSolves" (
                 participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 submission_id INTEGER NOT NULL
               );
               CREATE TABLE "Teams" (
                 id INTEGER PRIMARY KEY, name TEXT NOT NULL,
                 avatar_hash TEXT
               );
               CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY, user_name TEXT
               );
               INSERT INTO "AspNetUsers" (id, user_name) VALUES
                 ('00000000-0000-4000-8000-000000000117', 'old-name');
               INSERT INTO "Games" (id, end_time_utc, practice_mode) VALUES
                 (1, clock_timestamp() - interval '1 minute', FALSE),
                 (2, clock_timestamp() + interval '1 hour', FALSE),
                 (3, clock_timestamp() - interval '1 minute', TRUE);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
        let seeded: Vec<i32> = sqlx::query_scalar(
            r#"SELECT game_id FROM "FinalScoreboardMaterializations" ORDER BY game_id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(seeded, vec![1, 2]);
        sqlx::query(
            r#"INSERT INTO "Games" (id, end_time_utc, practice_mode) VALUES
                 (4, clock_timestamp() + interval '2 hours', FALSE),
                 (5, clock_timestamp() - interval '1 minute', TRUE)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let queued: Vec<i32> = sqlx::query_scalar(
            r#"SELECT game_id FROM "FinalScoreboardMaterializations" ORDER BY game_id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(queued, vec![1, 2, 4]);
        sqlx::query(
            r#"UPDATE "Games"
                  SET end_time_utc = end_time_utc + interval '1 hour'
                WHERE id = 4"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let due_matches: bool = sqlx::query_scalar(
            r#"SELECT finalization.available_at_utc = game.end_time_utc
                 FROM "FinalScoreboardMaterializations" finalization
                 JOIN "Games" game ON game.id = finalization.game_id
                WHERE finalization.game_id = 4"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(due_matches);
        sqlx::query(r#"UPDATE "Games" SET practice_mode = TRUE WHERE id = 4"#)
            .execute(&pool)
            .await
            .unwrap();
        let practice_queued: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "FinalScoreboardMaterializations" WHERE game_id = 4)"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!practice_queued);
        sqlx::query(
            r#"UPDATE "FinalScoreboardMaterializations"
                  SET invalidated_at_utc = clock_timestamp(),
                      completed_at_utc = clock_timestamp()
                WHERE game_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut mutation = pool.begin().await.unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" (id, game_id) VALUES (10, 1)"#)
            .execute(&mut *mutation)
            .await
            .unwrap();
        let pending_inside_mutation: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NULL AND invalidated_at_utc IS NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&mut *mutation)
        .await
        .unwrap();
        assert!(pending_inside_mutation);
        mutation.commit().await.unwrap();
        let pending_after_commit: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NULL AND invalidated_at_utc IS NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            pending_after_commit,
            "committed score mutation must not depend on post-commit cache code"
        );
        sqlx::query(
            r#"UPDATE "FinalScoreboardMaterializations"
                  SET invalidated_at_utc = clock_timestamp(),
                      completed_at_utc = clock_timestamp()
                WHERE game_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"UPDATE "GameChallenges" SET build_status = 1 WHERE id = 10"#)
            .execute(&pool)
            .await
            .unwrap();
        let build_update_kept_final: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NOT NULL AND invalidated_at_utc IS NOT NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            build_update_kept_final,
            "container build metadata must not requeue a final scoreboard"
        );
        sqlx::query(r#"UPDATE "GameChallenges" SET title = 'renamed' WHERE id = 10"#)
            .execute(&pool)
            .await
            .unwrap();
        let challenge_rename_requested_repair: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NULL AND invalidated_at_utc IS NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(challenge_rename_requested_repair);
        sqlx::query(
            r#"UPDATE "FinalScoreboardMaterializations"
                  SET invalidated_at_utc = clock_timestamp(),
                      completed_at_utc = clock_timestamp()
                WHERE game_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations"
                 (id, game_id, team_id, division_id, status, suspicion_score)
               VALUES (30, 1, 99, NULL, 0, 0)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "FinalScoreboardMaterializations"
                  SET invalidated_at_utc = clock_timestamp(),
                      completed_at_utc = clock_timestamp()
                WHERE game_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"UPDATE "Participations" SET suspicion_score = 17 WHERE id = 30"#)
            .execute(&pool)
            .await
            .unwrap();
        let suspicion_update_kept_final: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NOT NULL AND invalidated_at_utc IS NOT NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            suspicion_update_kept_final,
            "anti-cheat projection updates must not requeue scoreboards"
        );
        sqlx::query(r#"UPDATE "Participations" SET status = 1 WHERE id = 30"#)
            .execute(&pool)
            .await
            .unwrap();
        let status_update_requested_repair: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NULL AND invalidated_at_utc IS NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(status_update_requested_repair);
        sqlx::query(
            r#"UPDATE "FinalScoreboardMaterializations"
                  SET invalidated_at_utc = clock_timestamp(),
                      completed_at_utc = clock_timestamp()
                WHERE game_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "Submissions" (id, game_id, status) VALUES
                 (20, 1, 2)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let wrong_answer_kept_final: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NOT NULL AND invalidated_at_utc IS NOT NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            wrong_answer_kept_final,
            "wrong answers must not enqueue scoreboards on the submission hot path"
        );
        sqlx::query(
            r#"INSERT INTO "Submissions" (id, game_id, status) VALUES
                 (21, 1, 1)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let accepted_answer_requested_repair: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NULL AND invalidated_at_utc IS NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(accepted_answer_requested_repair);
        sqlx::query(
            r#"UPDATE "FinalScoreboardMaterializations"
                  SET invalidated_at_utc = clock_timestamp(),
                      completed_at_utc = clock_timestamp()
                WHERE game_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "Submissions"
                  SET user_id = '00000000-0000-4000-8000-000000000117'
                WHERE id = 21"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET user_name = 'new-name'
                WHERE id = '00000000-0000-4000-8000-000000000117'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let solver_rename_requested_repair: bool = sqlx::query_scalar(
            r#"SELECT completed_at_utc IS NULL AND invalidated_at_utc IS NULL
                 FROM "FinalScoreboardMaterializations" WHERE game_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(solver_rename_requested_repair);
        sqlx::query(r#"DELETE FROM "Games" WHERE id = 1"#)
            .execute(&pool)
            .await
            .unwrap();
        let retained: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*) FROM "FinalScoreboardMaterializations""#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(retained, 1, "game deletion must cascade only its queue row");

        Migration.down(&manager).await.unwrap();
        let table_exists: bool = sqlx::query_scalar(
            r#"SELECT to_regclass('"FinalScoreboardMaterializations"') IS NOT NULL"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            table_exists,
            "forward-only rollback must retain durable finalization state"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
