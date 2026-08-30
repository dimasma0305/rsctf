//! Durable retry identity and cache generations for the KotH referee API.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "KothApiObservationOperations" (
    challenge_id INTEGER NOT NULL,
    game_id INTEGER NOT NULL,
    request_digest BYTEA NOT NULL,
    signer_scope VARCHAR(96) NOT NULL,
    body_digest BYTEA NOT NULL,
    context_hash CHAR(64) NOT NULL,
    lease_token UUID NOT NULL,
    lease_expires_at TIMESTAMPTZ NOT NULL,
    response JSONB NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ NULL,
    expires_at TIMESTAMPTZ NOT NULL
        DEFAULT (clock_timestamp() + interval '10 minutes'),
    PRIMARY KEY (challenge_id, request_digest),
    CONSTRAINT fk_koth_observation_operations_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_observation_operations_request_digest
        CHECK (OCTET_LENGTH(request_digest) = 32),
    CONSTRAINT ck_koth_observation_operations_body_digest
        CHECK (OCTET_LENGTH(body_digest) = 32),
    CONSTRAINT ck_koth_observation_operations_signer
        CHECK (BTRIM(signer_scope) <> ''),
    CONSTRAINT ck_koth_observation_operations_context
        CHECK (context_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_koth_observation_operations_completion
        CHECK (
            (response IS NULL AND completed_at IS NULL)
            OR
            (response IS NOT NULL AND completed_at IS NOT NULL
             AND jsonb_typeof(response) = 'object')
        ),
    CONSTRAINT ck_koth_observation_operations_expiry
        CHECK (expires_at > created_at)
);

-- Development builds of the earlier prototype used the same table name
-- without the authenticated signer/body fields. Those short-lived leases
-- cannot be upgraded safely, so discard only incompatible ephemeral rows and
-- converge the table shape before serving traffic.
ALTER TABLE "KothApiObservationOperations"
    ADD COLUMN IF NOT EXISTS signer_scope VARCHAR(96),
    ADD COLUMN IF NOT EXISTS body_digest BYTEA;
DELETE FROM "KothApiObservationOperations"
 WHERE signer_scope IS NULL OR body_digest IS NULL;
ALTER TABLE "KothApiObservationOperations"
    ALTER COLUMN signer_scope SET NOT NULL,
    ALTER COLUMN body_digest SET NOT NULL;
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conrelid = '"KothApiObservationOperations"'::regclass
       AND conname = 'ck_koth_observation_operations_request_digest'
  ) THEN
    ALTER TABLE "KothApiObservationOperations"
      ADD CONSTRAINT ck_koth_observation_operations_request_digest
      CHECK (OCTET_LENGTH(request_digest) = 32);
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conrelid = '"KothApiObservationOperations"'::regclass
       AND conname = 'ck_koth_observation_operations_body_digest'
  ) THEN
    ALTER TABLE "KothApiObservationOperations"
      ADD CONSTRAINT ck_koth_observation_operations_body_digest
      CHECK (OCTET_LENGTH(body_digest) = 32);
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conrelid = '"KothApiObservationOperations"'::regclass
       AND conname = 'ck_koth_observation_operations_signer'
  ) THEN
    ALTER TABLE "KothApiObservationOperations"
      ADD CONSTRAINT ck_koth_observation_operations_signer
      CHECK (BTRIM(signer_scope) <> '');
  END IF;
END
$$;

CREATE INDEX IF NOT EXISTS ix_koth_observation_operations_expiry
    ON "KothApiObservationOperations"(expires_at);
CREATE INDEX IF NOT EXISTS ix_koth_observation_operations_scope
    ON "KothApiObservationOperations"(game_id, challenge_id, created_at DESC);

CREATE TABLE IF NOT EXISTS "KothObserverContextGenerations" (
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1,
    generated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (game_id, challenge_id),
    CONSTRAINT fk_koth_context_generations_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_context_generations_generation
        CHECK (generation BETWEEN 1 AND 9007199254740991)
);

ALTER TABLE "KothObserverContextGenerations"
    ADD COLUMN IF NOT EXISTS generated_at TIMESTAMPTZ NOT NULL
        DEFAULT clock_timestamp();
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conrelid = '"KothObserverContextGenerations"'::regclass
       AND conname = 'fk_koth_context_generations_challenge'
  ) THEN
    ALTER TABLE "KothObserverContextGenerations"
      ADD CONSTRAINT fk_koth_context_generations_challenge
      FOREIGN KEY (game_id, challenge_id)
      REFERENCES "GameChallenges"(game_id, id)
      ON DELETE CASCADE;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conrelid = '"KothObserverContextGenerations"'::regclass
       AND conname = 'ck_koth_context_generations_generation'
  ) THEN
    ALTER TABLE "KothObserverContextGenerations"
      ADD CONSTRAINT ck_koth_context_generations_generation
      CHECK (generation BETWEEN 1 AND 9007199254740991);
  END IF;
END
$$;

INSERT INTO "KothObserverContextGenerations" (game_id, challenge_id, generation)
SELECT game_id, challenge_id, 1 FROM "KothApiObservers"
ON CONFLICT (game_id, challenge_id) DO NOTHING;

CREATE OR REPLACE FUNCTION bump_koth_context_pair(
    target_game INTEGER,
    target_challenge INTEGER
)
RETURNS VOID LANGUAGE SQL AS $$
  INSERT INTO "KothObserverContextGenerations" (game_id, challenge_id, generation)
  SELECT target_game, target_challenge, 2
    FROM "GameChallenges" challenge
   WHERE challenge.game_id = target_game
     AND challenge.id = target_challenge
  ON CONFLICT (game_id, challenge_id) DO UPDATE
     SET generation = LEAST(
         "KothObserverContextGenerations".generation + 1,
         9007199254740991
     ),
         generated_at = clock_timestamp();
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_pair()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    PERFORM bump_koth_context_pair(OLD.game_id, OLD.challenge_id);
  ELSE
    PERFORM bump_koth_context_pair(NEW.game_id, NEW.challenge_id);
    IF TG_OP = 'UPDATE'
       AND (NEW.game_id, NEW.challenge_id)
           IS DISTINCT FROM (OLD.game_id, OLD.challenge_id) THEN
      PERFORM bump_koth_context_pair(OLD.game_id, OLD.challenge_id);
    END IF;
  END IF;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_game_id()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
  new_game INTEGER;
  old_game INTEGER;
BEGIN
  IF TG_OP <> 'DELETE' THEN
    new_game := NEW.game_id;
    UPDATE "KothObserverContextGenerations"
       SET generation = LEAST(generation + 1, 9007199254740991),
           generated_at = clock_timestamp()
     WHERE game_id = new_game;
  END IF;
  IF TG_OP = 'DELETE'
     OR (TG_OP = 'UPDATE' AND NEW.game_id IS DISTINCT FROM OLD.game_id) THEN
    old_game := OLD.game_id;
    UPDATE "KothObserverContextGenerations"
       SET generation = LEAST(generation + 1, 9007199254740991),
           generated_at = clock_timestamp()
     WHERE game_id = old_game;
  END IF;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_game_row()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE "KothObserverContextGenerations"
     SET generation = LEAST(generation + 1, 9007199254740991),
         generated_at = clock_timestamp()
   WHERE game_id = COALESCE(NEW.id, OLD.id);
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_team_member()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
  changed_team INTEGER;
BEGIN
  IF TG_OP = 'UPDATE'
     AND (NEW.team_id, NEW.user_id) IS NOT DISTINCT FROM (OLD.team_id, OLD.user_id) THEN
    RETURN NEW;
  END IF;
  changed_team := COALESCE(NEW.team_id, OLD.team_id);
  UPDATE "KothObserverContextGenerations" generation
     SET generation = LEAST(generation.generation + 1, 9007199254740991),
         generated_at = clock_timestamp()
   WHERE generation.game_id IN (
     SELECT participation.game_id
       FROM "Participations" participation
      WHERE participation.team_id = changed_team
   );
  IF TG_OP = 'UPDATE' AND NEW.team_id IS DISTINCT FROM OLD.team_id THEN
    UPDATE "KothObserverContextGenerations" generation
       SET generation = LEAST(generation.generation + 1, 9007199254740991),
           generated_at = clock_timestamp()
     WHERE generation.game_id IN (
       SELECT participation.game_id
         FROM "Participations" participation
        WHERE participation.team_id = OLD.team_id
     );
  END IF;
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_team()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE'
     AND (NEW.captain_id, NEW.deletion_pending)
         IS NOT DISTINCT FROM (OLD.captain_id, OLD.deletion_pending) THEN
    RETURN NEW;
  END IF;
  UPDATE "KothObserverContextGenerations" generation
     SET generation = LEAST(generation.generation + 1, 9007199254740991),
         generated_at = clock_timestamp()
   WHERE generation.game_id IN (
     SELECT participation.game_id
       FROM "Participations" participation
      WHERE participation.team_id = COALESCE(NEW.id, OLD.id)
   );
  RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE OR REPLACE FUNCTION bump_koth_context_from_account()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE "KothObserverContextGenerations" generation
     SET generation = LEAST(generation.generation + 1, 9007199254740991),
         generated_at = clock_timestamp()
   WHERE generation.game_id IN (
     SELECT participation.game_id
       FROM "Participations" participation
       JOIN "Teams" team ON team.id = participation.team_id
      WHERE team.captain_id = COALESCE(NEW.id, OLD.id)
         OR EXISTS (
              SELECT 1
                FROM "TeamMembers" member
               WHERE member.team_id = team.id
                 AND member.user_id = COALESCE(NEW.id, OLD.id)
            )
   );
  RETURN COALESCE(NEW, OLD);
END;
$$;

DROP TRIGGER IF EXISTS tr_koth_context_observer_insert_delete ON "KothApiObservers";
CREATE TRIGGER tr_koth_context_observer_insert_delete
AFTER INSERT OR DELETE ON "KothApiObservers"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_pair();
DROP TRIGGER IF EXISTS tr_koth_context_observer_secret ON "KothApiObservers";
CREATE TRIGGER tr_koth_context_observer_secret
AFTER UPDATE OF game_id, challenge_id, hmac_secret ON "KothApiObservers"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.challenge_id, NEW.hmac_secret)
      IS DISTINCT FROM (OLD.game_id, OLD.challenge_id, OLD.hmac_secret))
EXECUTE FUNCTION bump_koth_context_from_pair();

DROP TRIGGER IF EXISTS tr_koth_context_observer_revision_insert_delete
    ON "KothApiObserverRevisions";
CREATE TRIGGER tr_koth_context_observer_revision_insert_delete
AFTER INSERT OR DELETE ON "KothApiObserverRevisions"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_pair();
DROP TRIGGER IF EXISTS tr_koth_context_observer_revision_update
    ON "KothApiObserverRevisions";
CREATE TRIGGER tr_koth_context_observer_revision_update
AFTER UPDATE OF game_id, challenge_id, revision ON "KothApiObserverRevisions"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.challenge_id, NEW.revision)
      IS DISTINCT FROM (OLD.game_id, OLD.challenge_id, OLD.revision))
EXECUTE FUNCTION bump_koth_context_from_pair();

DROP TRIGGER IF EXISTS tr_koth_context_token_insert_delete ON "KothApiTeamTokens";
CREATE TRIGGER tr_koth_context_token_insert_delete
AFTER INSERT OR DELETE ON "KothApiTeamTokens"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_pair();
DROP TRIGGER IF EXISTS tr_koth_context_token_update ON "KothApiTeamTokens";
CREATE TRIGGER tr_koth_context_token_update
AFTER UPDATE OF game_id, challenge_id, participation_id, token, generation
ON "KothApiTeamTokens"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.challenge_id, NEW.participation_id, NEW.token, NEW.generation)
      IS DISTINCT FROM
      (OLD.game_id, OLD.challenge_id, OLD.participation_id, OLD.token, OLD.generation))
EXECUTE FUNCTION bump_koth_context_from_pair();

DROP TRIGGER IF EXISTS tr_koth_context_target_insert_delete ON "KothTargets";
CREATE TRIGGER tr_koth_context_target_insert_delete
AFTER INSERT OR DELETE ON "KothTargets"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_pair();
DROP TRIGGER IF EXISTS tr_koth_context_target_update ON "KothTargets";
CREATE TRIGGER tr_koth_context_target_update
AFTER UPDATE OF game_id, challenge_id, container_id ON "KothTargets"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.challenge_id, NEW.container_id)
      IS DISTINCT FROM (OLD.game_id, OLD.challenge_id, OLD.container_id))
EXECUTE FUNCTION bump_koth_context_from_pair();

DROP TRIGGER IF EXISTS tr_koth_context_cycle_insert_delete ON "KothCrownCycles";
CREATE TRIGGER tr_koth_context_cycle_insert_delete
AFTER INSERT OR DELETE ON "KothCrownCycles"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_pair();
DROP TRIGGER IF EXISTS tr_koth_context_cycle_update ON "KothCrownCycles";
CREATE TRIGGER tr_koth_context_cycle_update
AFTER UPDATE OF game_id, challenge_id, cycle_number, reset_attempt,
                replacement_container_id, phase
ON "KothCrownCycles"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.challenge_id, NEW.cycle_number, NEW.reset_attempt,
       NEW.replacement_container_id, NEW.phase)
      IS DISTINCT FROM
      (OLD.game_id, OLD.challenge_id, OLD.cycle_number, OLD.reset_attempt,
       OLD.replacement_container_id, OLD.phase))
EXECUTE FUNCTION bump_koth_context_from_pair();

DROP TRIGGER IF EXISTS tr_koth_context_scheme_insert_delete ON "KothApiArenaSchemes";
CREATE TRIGGER tr_koth_context_scheme_insert_delete
AFTER INSERT OR DELETE ON "KothApiArenaSchemes"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_pair();
DROP TRIGGER IF EXISTS tr_koth_context_scheme_update ON "KothApiArenaSchemes";
CREATE TRIGGER tr_koth_context_scheme_update
AFTER UPDATE OF game_id, challenge_id, objective_ids, objective_schema_hash
ON "KothApiArenaSchemes"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.challenge_id, NEW.objective_ids, NEW.objective_schema_hash)
      IS DISTINCT FROM
      (OLD.game_id, OLD.challenge_id, OLD.objective_ids, OLD.objective_schema_hash))
EXECUTE FUNCTION bump_koth_context_from_pair();

DROP TRIGGER IF EXISTS tr_koth_context_round_insert_delete ON "AdRounds";
CREATE TRIGGER tr_koth_context_round_insert_delete
AFTER INSERT OR DELETE ON "AdRounds"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_game_id();
DROP TRIGGER IF EXISTS tr_koth_context_round_update ON "AdRounds";
CREATE TRIGGER tr_koth_context_round_update
AFTER UPDATE OF game_id, number, start_time_utc, end_time_utc, finalized
ON "AdRounds"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.number, NEW.start_time_utc, NEW.end_time_utc, NEW.finalized)
      IS DISTINCT FROM
      (OLD.game_id, OLD.number, OLD.start_time_utc, OLD.end_time_utc, OLD.finalized))
EXECUTE FUNCTION bump_koth_context_from_game_id();

DROP TRIGGER IF EXISTS tr_koth_context_config_insert_delete ON "KothOfficialConfigs";
CREATE TRIGGER tr_koth_context_config_insert_delete
AFTER INSERT OR DELETE ON "KothOfficialConfigs"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_game_id();
DROP TRIGGER IF EXISTS tr_koth_context_config_update ON "KothOfficialConfigs";
CREATE TRIGGER tr_koth_context_config_update
AFTER UPDATE OF game_id, roster_snapshot, hills_snapshot ON "KothOfficialConfigs"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.roster_snapshot, NEW.hills_snapshot)
      IS DISTINCT FROM (OLD.game_id, OLD.roster_snapshot, OLD.hills_snapshot))
EXECUTE FUNCTION bump_koth_context_from_game_id();

DROP TRIGGER IF EXISTS tr_koth_context_participation_insert_delete ON "Participations";
CREATE TRIGGER tr_koth_context_participation_insert_delete
AFTER INSERT OR DELETE ON "Participations"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_game_id();
DROP TRIGGER IF EXISTS tr_koth_context_participation_update ON "Participations";
CREATE TRIGGER tr_koth_context_participation_update
AFTER UPDATE OF game_id, team_id, status ON "Participations"
FOR EACH ROW
WHEN ((NEW.game_id, NEW.team_id, NEW.status)
      IS DISTINCT FROM (OLD.game_id, OLD.team_id, OLD.status))
EXECUTE FUNCTION bump_koth_context_from_game_id();

DROP TRIGGER IF EXISTS tr_koth_context_game_window ON "Games";
CREATE TRIGGER tr_koth_context_game_window
AFTER UPDATE OF start_time_utc, end_time_utc ON "Games"
FOR EACH ROW
WHEN ((NEW.start_time_utc, NEW.end_time_utc)
      IS DISTINCT FROM (OLD.start_time_utc, OLD.end_time_utc))
EXECUTE FUNCTION bump_koth_context_from_game_row();

DROP TRIGGER IF EXISTS tr_koth_context_team_member ON "TeamMembers";
CREATE TRIGGER tr_koth_context_team_member
AFTER INSERT OR UPDATE OR DELETE ON "TeamMembers"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_team_member();

DROP TRIGGER IF EXISTS tr_koth_context_team ON "Teams";
CREATE TRIGGER tr_koth_context_team
AFTER UPDATE OF captain_id, deletion_pending OR DELETE ON "Teams"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_team();

DROP TRIGGER IF EXISTS tr_koth_context_account_role ON "AspNetUsers";
CREATE TRIGGER tr_koth_context_account_role
AFTER UPDATE OF role ON "AspNetUsers"
FOR EACH ROW WHEN (NEW.role IS DISTINCT FROM OLD.role)
EXECUTE FUNCTION bump_koth_context_from_account();
DROP TRIGGER IF EXISTS tr_koth_context_account_delete ON "AspNetUsers";
CREATE TRIGGER tr_koth_context_account_delete
AFTER DELETE ON "AspNetUsers"
FOR EACH ROW EXECUTE FUNCTION bump_koth_context_from_account();

-- Close the migration-time race between the initial backfill and trigger
-- installation. Rows inserted by an old replica during that window are now
-- covered either by this second pass or by the installed INSERT trigger.
INSERT INTO "KothObserverContextGenerations" (game_id, challenge_id, generation)
SELECT game_id, challenge_id, 1 FROM "KothApiObservers"
ON CONFLICT (game_id, challenge_id) DO NOTHING;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

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
    use sea_orm_migration::{MigrationTrait, SchemaManager};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::Migration;
    use super::UP_SQL;

    #[test]
    fn observation_operations_are_atomic_bounded_and_reclaimable() {
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, request_digest)"));
        assert!(UP_SQL.contains("response JSONB NULL"));
        assert!(UP_SQL.contains("lease_expires_at TIMESTAMPTZ NOT NULL"));
        assert!(UP_SQL.contains("interval '10 minutes'"));
        assert!(UP_SQL.contains("ix_koth_observation_operations_expiry"));
        assert!(UP_SQL.contains("ON DELETE CASCADE"));
    }

    #[test]
    fn context_generation_excludes_noisy_last_used_updates() {
        for source in [
            "KothApiObserverRevisions",
            "KothApiTeamTokens",
            "KothTargets",
            "KothCrownCycles",
            "KothApiArenaSchemes",
            "AdRounds",
            "KothOfficialConfigs",
            "Participations",
            "TeamMembers",
            "Teams",
            "AspNetUsers",
        ] {
            assert!(
                UP_SQL.contains(source),
                "missing invalidation source {source}"
            );
        }
        assert!(!UP_SQL.contains("UPDATE OF last_used_at"));
        assert!(UP_SQL.contains("FROM \"GameChallenges\" challenge"));
        assert!(UP_SQL.contains("AFTER UPDATE OF role ON \"AspNetUsers\""));
        assert!(UP_SQL.contains("WHEN (NEW.role IS DISTINCT FROM OLD.role)"));
        assert!(UP_SQL.contains("NEW.replacement_container_id, NEW.phase)"));
        assert_eq!(
            UP_SQL
                .matches("SELECT game_id, challenge_id, 1 FROM \"KothApiObservers\"")
                .count(),
            2
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_installs_idempotent_triggers_and_only_semantic_changes_advance_context() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0251_{}", uuid::Uuid::new_v4().simple());
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
            r#"CREATE TABLE "Games" (
                 id INTEGER PRIMARY KEY,
                 start_time_utc TIMESTAMPTZ NOT NULL,
                 end_time_utc TIMESTAMPTZ NOT NULL
               );
               CREATE TABLE "GameChallenges" (
                 id INTEGER PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 UNIQUE (game_id, id)
               );
               CREATE TABLE "KothApiObservers" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 hmac_secret TEXT NOT NULL, last_used_at TIMESTAMPTZ,
                 FOREIGN KEY (game_id, challenge_id)
                   REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE
               );
               CREATE TABLE "KothApiObserverRevisions" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 revision BIGINT NOT NULL
               );
               CREATE TABLE "KothApiTeamTokens" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 participation_id INTEGER NOT NULL, token TEXT NOT NULL,
                 generation INTEGER NOT NULL, last_used_at TIMESTAMPTZ
               );
               CREATE TABLE "KothTargets" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 container_id TEXT
               );
               CREATE TABLE "KothCrownCycles" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 cycle_number INTEGER NOT NULL, reset_attempt INTEGER NOT NULL,
                 replacement_container_id TEXT, phase TEXT NOT NULL
               );
               CREATE TABLE "KothApiArenaSchemes" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 objective_ids TEXT[], objective_schema_hash BYTEA
               );
               CREATE TABLE "AdRounds" (
                 game_id INTEGER NOT NULL, number INTEGER NOT NULL,
                 start_time_utc TIMESTAMPTZ NOT NULL,
                 end_time_utc TIMESTAMPTZ NOT NULL, finalized BOOLEAN NOT NULL
               );
               CREATE TABLE "KothOfficialConfigs" (
                 game_id INTEGER NOT NULL, roster_snapshot JSONB NOT NULL,
                 hills_snapshot JSONB NOT NULL
               );
               CREATE TABLE "Participations" (
                 game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
                 status SMALLINT NOT NULL
               );
               CREATE TABLE "Teams" (
                 id INTEGER PRIMARY KEY, captain_id UUID,
                 deletion_pending BOOLEAN NOT NULL
               );
               CREATE TABLE "TeamMembers" (
                 team_id INTEGER NOT NULL, user_id UUID NOT NULL
               );
               CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY, role SMALLINT NOT NULL,
                 last_visited_utc TIMESTAMPTZ NOT NULL
               );

               INSERT INTO "Games" VALUES
                 (7, clock_timestamp() - interval '1 hour',
                     clock_timestamp() + interval '1 hour');
               INSERT INTO "GameChallenges" VALUES (9, 7);
               INSERT INTO "KothApiObservers" VALUES (7, 9, 'secret', NULL);
               INSERT INTO "KothApiObserverRevisions" VALUES (7, 9, 1);
               INSERT INTO "KothApiTeamTokens" VALUES
                 (7, 9, 11, 'token-a', 1, NULL);
               INSERT INTO "KothTargets" VALUES (7, 9, 'runtime-a');
               INSERT INTO "KothCrownCycles" VALUES
                 (7, 9, 1, 0, 'runtime-a', 'Active');
               INSERT INTO "KothApiArenaSchemes" VALUES
                 (7, 9, ARRAY['quality'], decode(repeat('11', 32), 'hex'));
               INSERT INTO "AdRounds" VALUES
                 (7, 1, clock_timestamp() - interval '1 minute',
                     clock_timestamp() + interval '1 minute', FALSE);
               INSERT INTO "KothOfficialConfigs" VALUES
                 (7, '[11]', '[{"challengeId":9,"claimSource":"Api"}]');
               INSERT INTO "AspNetUsers" VALUES
                 ('00000000-0000-4000-8000-000000000251', 1, clock_timestamp());
               INSERT INTO "Teams" VALUES
                 (21, '00000000-0000-4000-8000-000000000251', FALSE);
               INSERT INTO "Participations" VALUES (7, 21, 1);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        async fn generation(pool: &sqlx::PgPool) -> i64 {
            sqlx::query_scalar(
                r#"SELECT generation FROM "KothObserverContextGenerations"
                    WHERE game_id = 7 AND challenge_id = 9"#,
            )
            .fetch_one(pool)
            .await
            .unwrap()
        }

        assert_eq!(generation(&pool).await, 1);
        sqlx::query(
            r#"UPDATE "KothApiObservers"
                  SET last_used_at = clock_timestamp()
                WHERE game_id = 7 AND challenge_id = 9"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "KothApiTeamTokens"
                  SET last_used_at = clock_timestamp()
                WHERE game_id = 7 AND challenge_id = 9"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "AspNetUsers"
                  SET last_visited_utc = clock_timestamp(), role = role
                WHERE id = '00000000-0000-4000-8000-000000000251'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "KothCrownCycles"
                  SET phase = phase,
                      replacement_container_id = replacement_container_id,
                      reset_attempt = reset_attempt
                WHERE game_id = 7 AND challenge_id = 9"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "KothApiTeamTokens" SET token = token, generation = generation
                WHERE game_id = 7 AND challenge_id = 9"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation(&pool).await, 1);

        sqlx::query(
            r#"UPDATE "KothApiTeamTokens" SET token = 'token-b', generation = 2
                WHERE game_id = 7 AND challenge_id = 9"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation(&pool).await, 2);
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET role = 2
                WHERE id = '00000000-0000-4000-8000-000000000251'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation(&pool).await, 3);
        sqlx::query(
            r#"INSERT INTO "TeamMembers" VALUES
                 (21, '00000000-0000-4000-8000-000000000251')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation(&pool).await, 4);
        sqlx::query(
            r#"DELETE FROM "TeamMembers"
                WHERE team_id = 21
                  AND user_id = '00000000-0000-4000-8000-000000000251'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation(&pool).await, 5);
        sqlx::query(
            r#"UPDATE "Games" SET end_time_utc = end_time_utc + interval '1 minute'
                WHERE id = 7"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(generation(&pool).await, 6);

        sqlx::query(r#"DELETE FROM "GameChallenges" WHERE game_id = 7 AND id = 9"#)
            .execute(&pool)
            .await
            .unwrap();
        let generation_after_cascade: Option<i64> = sqlx::query_scalar(
            r#"SELECT generation FROM "KothObserverContextGenerations"
                WHERE game_id = 7 AND challenge_id = 9"#,
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(generation_after_cascade, None);
        let observers_after_cascade: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*) FROM "KothApiObservers""#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(observers_after_cascade, 0);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
