//! Canonical stolen-flag provenance and crash-recoverable suspicion evaluation.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
-- Defense in depth for direct Migrator::up callers: refuse the compatibility
-- boundary before any DDL unless the deployment has drained every other
-- database client. Connections from this process share one unguessable name.
DO $$
DECLARE
  own_application_name TEXT := current_setting('application_name');
  other_clients BIGINT;
BEGIN
  IF own_application_name !~ '^rsctf:' THEN
    RAISE EXCEPTION
      'm0089 requires rsctf process-unique PostgreSQL application_name';
  END IF;
  SELECT COUNT(*)::bigint
    INTO other_clients
    FROM pg_stat_activity other
   WHERE other.datid = (
           SELECT oid FROM pg_database WHERE datname = current_database()
         )
     AND other.pid <> pg_backend_pid()
     AND other.usesysid IS NOT NULL
     AND other.application_name IS DISTINCT FROM own_application_name;
  IF other_clients <> 0 THEN
    RAISE EXCEPTION
      'm0089 exclusive schema cutover refused: % other database client session(s)',
      other_clients
      USING HINT = 'Scale every rsctf role to zero and drain PgBouncer, monitors, and administrative sessions.';
  END IF;
END
$$;

-- Drain in-flight gameplay writers before taking any child-table lock. Every
-- submit/evidence writer takes Games FOR SHARE before its first durable write;
-- taking a child first could deadlock with an old replica during a rolling
-- upgrade (migration waits on Submission while submit waits on GameChallenge).
LOCK TABLE "Games" IN EXCLUSIVE MODE;
LOCK TABLE "Teams", "AspNetUsers", "Participations", "GameChallenges",
           "GameInstances", "FlagContexts", "GameEvents", "Submissions", "FirstSolves",
           "CheatInfo", "ContainerAccessEvents", "HoneypotHits",
           "SuspicionEvents"
  IN SHARE ROW EXCLUSIVE MODE;

-- A manual idempotence/recovery rerun may encounter the triggers installed by
-- a prior successful pass. The table lock keeps the brief rebuild window private.
DROP TRIGGER IF EXISTS trg_cheatinfo_immutable ON "CheatInfo";
DROP TRIGGER IF EXISTS trg_cheatinfo_validate_insert ON "CheatInfo";
DROP TRIGGER IF EXISTS trg_cheat_submission_immutable_update ON "Submissions";
DROP TRIGGER IF EXISTS trg_cheat_submission_immutable_delete ON "Submissions";
DROP TRIGGER IF EXISTS trg_submission_core_immutable_update ON "Submissions";
DROP TRIGGER IF EXISTS trg_submission_core_immutable_delete ON "Submissions";
DROP TRIGGER IF EXISTS trg_submissions_immutable_observations ON "Submissions";
DROP TRIGGER IF EXISTS trg_firstsolve_validate_insert ON "FirstSolves";
DROP TRIGGER IF EXISTS trg_firstsolve_immutable ON "FirstSolves";
DROP TRIGGER IF EXISTS trg_containeraccess_immutable ON "ContainerAccessEvents";
DROP TRIGGER IF EXISTS trg_submissions_snapshot_observations ON "Submissions";
DROP TRIGGER IF EXISTS trg_submissions_evaluation_outbox ON "Submissions";
DROP TRIGGER IF EXISTS trg_participations_competitive_admission ON "Participations";
DROP TRIGGER IF EXISTS trg_games_seed_suspicion_reconciliation ON "Games";
DROP FUNCTION IF EXISTS rsctf_reject_cheat_submission_mutation();
DROP FUNCTION IF EXISTS rsctf_reject_submission_observation_mutation();

ALTER TABLE "CheatInfo"
  ADD COLUMN IF NOT EXISTS submit_participation_id INTEGER,
  ADD COLUMN IF NOT EXISTS source_participation_id INTEGER,
  ADD COLUMN IF NOT EXISTS challenge_id INTEGER,
  ADD COLUMN IF NOT EXISTS evidence_key TEXT,
  ADD COLUMN IF NOT EXISTS observed_at_utc TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS evidence_payload JSONB,
  ADD COLUMN IF NOT EXISTS evidence_version SMALLINT;
-- SeaORM's schema-derived fresh install historically materialized `Json` as
-- PostgreSQL JSON. Normalize it to JSONB before installing JSONB checks; an
-- upgraded deployment already has the desired type and this remains a no-op.
ALTER TABLE "CheatInfo"
  ALTER COLUMN evidence_payload TYPE JSONB USING evidence_payload::jsonb;

ALTER TABLE "Submissions"
  ADD COLUMN IF NOT EXISTS submit_remote_ip_hash BYTEA,
  ADD COLUMN IF NOT EXISTS container_id UUID,
  ADD COLUMN IF NOT EXISTS container_last_operation_at_submit TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS container_was_loaded_at_submit BOOLEAN,
  ADD COLUMN IF NOT EXISTS first_open_at_submit TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS first_download_at_submit TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS first_container_start_at_submit TIMESTAMPTZ;
ALTER TABLE "Participations"
  ADD COLUMN IF NOT EXISTS competitive_admitted_at_utc TIMESTAMPTZ;
ALTER TABLE "ContainerAccessEvents"
  ADD COLUMN IF NOT EXISTS remote_ip_hash BYTEA,
  ADD COLUMN IF NOT EXISTS is_monitor BOOLEAN;

-- accepted_count is the number of distinct canonical solves, not the number
-- of accepted attempts. Repair historical replay inflation from FirstSolves,
-- rejecting any malformed row whose submission identity does not line up with
-- the challenge, participation, game, and Accepted grade.
UPDATE "GameChallenges" challenge
   SET accepted_count = (
         SELECT COUNT(*)::integer
           FROM "FirstSolves" first_solve
           JOIN "Submissions" submission
             ON submission.id = first_solve.submission_id
            AND submission.participation_id = first_solve.participation_id
            AND submission.challenge_id = first_solve.challenge_id
            AND submission.game_id = challenge.game_id
            AND submission.status = 1
           JOIN "Participations" participation
             ON participation.id = first_solve.participation_id
            AND participation.game_id = challenge.game_id
          WHERE first_solve.challenge_id = challenge.id
       );

-- The submission is the canonical submit-side identity. Repair the redundant
-- legacy game/team fields rather than blessing a mismatched historical row.
UPDATE "CheatInfo" cheat
   SET game_id = submission.game_id,
       submit_team_id = submission.team_id,
       submit_participation_id = submission.participation_id,
       challenge_id = submission.challenge_id,
       observed_at_utc = submission.submit_time_utc,
       evidence_key = 'submission:' || submission.id::text
 FROM "Submissions" submission
 WHERE submission.id = cheat.submission_id;

-- A legacy provenance row is meaningful only when its canonical submission
-- was itself graded CheatDetected. Fail the whole migration rather than make
-- corrupt historical metadata immutable under a contract new writes reject.
DO $$
BEGIN
  IF EXISTS (
    SELECT 1
      FROM "CheatInfo" cheat
      LEFT JOIN "Submissions" submission ON submission.id = cheat.submission_id
     WHERE submission.id IS NULL OR submission.status <> 3
  ) THEN
    RAISE EXCEPTION
      'cannot canonicalize CheatInfo without a CheatDetected submission'
      USING HINT = 'Repair or remove corrupt legacy CheatInfo rows before retrying m0089.';
  END IF;
END
$$;

-- Old CheatInfo only named the source team. Prefer a participation whose
-- challenge instance still proves the submitted flag; otherwise choose the
-- oldest same-game participation deterministically. Never invent an identity.
UPDATE "CheatInfo" cheat
   SET source_participation_id = (
         SELECT source.id
           FROM "Participations" source
           LEFT JOIN "GameInstances" instance
             ON instance.participation_id = source.id
            AND instance.challenge_id = submission.challenge_id
           LEFT JOIN "FlagContexts" flag ON flag.id = instance.flag_id
          WHERE source.game_id = submission.game_id
            AND source.team_id = cheat.source_team_id
          ORDER BY (flag.flag = submission.answer) DESC NULLS LAST, source.id
          LIMIT 1
       )
  FROM "Submissions" submission
 WHERE submission.id = cheat.submission_id
   AND cheat.source_participation_id IS NULL;

UPDATE "CheatInfo" cheat
   SET evidence_payload = jsonb_build_object(
         'challengeTitle', challenge.title,
         'submitTeamName', submit_team.name,
         'sourceTeamName', source_team.name,
         'submitUserName', COALESCE(account.user_name, '')
       ),
       evidence_version = 1
  FROM "Submissions" submission
  JOIN "GameChallenges" challenge ON challenge.id = submission.challenge_id
  JOIN "Teams" submit_team ON submit_team.id = submission.team_id
  CROSS JOIN "Teams" source_team
  LEFT JOIN "AspNetUsers" account ON account.id = submission.user_id
 WHERE submission.id = cheat.submission_id
   AND source_team.id = cheat.source_team_id
   AND (cheat.evidence_version IS NULL OR cheat.evidence_payload IS NULL);

-- Freeze the competitive population. An in-window submission proves admission
-- regardless of mutable review status. Without one, only the currently
-- accepted/suspended roster of a still-open game is safely observable;
-- unknown rows remain NULL rather than inventing a denominator.
WITH migration_clock AS MATERIALIZED (
  SELECT clock_timestamp() AS db_now
)
UPDATE "Participations" participation
   SET competitive_admitted_at_utc = COALESCE(
         (
           SELECT MIN(submission.submit_time_utc)
             FROM "Submissions" submission
            WHERE submission.participation_id = participation.id
              AND submission.game_id = participation.game_id
              AND submission.submit_time_utc >= game.start_time_utc
              AND submission.submit_time_utc < game.end_time_utc
         ),
         CASE WHEN game.end_time_utc > migration_clock.db_now
                    AND participation.status IN (1, 3)
              THEN migration_clock.db_now END
       )
  FROM "Games" game
  CROSS JOIN migration_clock
 WHERE participation.game_id = game.id
   AND participation.competitive_admitted_at_utc IS NULL
   AND (
     (game.end_time_utc > migration_clock.db_now
       AND participation.status IN (1, 3))
     OR EXISTS (
       SELECT 1 FROM "Submissions" submission
        WHERE submission.participation_id = participation.id
          AND submission.game_id = participation.game_id
          AND submission.submit_time_utc >= game.start_time_utc
          AND submission.submit_time_utc < game.end_time_utc
     )
   );

-- Legacy duplicate rows represented one submission more than once. Keep the
-- first canonical row; from this point onward the ledger is one-to-one and
-- database-enforced immutable.
WITH ranked AS (
  SELECT id,
         ROW_NUMBER() OVER (PARTITION BY submission_id ORDER BY id) AS position
    FROM "CheatInfo"
)
DELETE FROM "CheatInfo" cheat
 USING ranked
 WHERE ranked.id = cheat.id
   AND ranked.position > 1;

DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM "CheatInfo"
     WHERE submit_participation_id IS NULL
        OR source_participation_id IS NULL
        OR challenge_id IS NULL
        OR evidence_key IS NULL
        OR observed_at_utc IS NULL
        OR evidence_payload IS NULL
        OR evidence_version IS NULL
  ) THEN
    RAISE EXCEPTION
      'cannot canonicalize legacy CheatInfo provenance'
      USING HINT = 'Repair orphaned legacy CheatInfo rows before retrying m0089.';
  END IF;
END
$$;

ALTER TABLE "CheatInfo"
  ALTER COLUMN submit_participation_id SET NOT NULL,
  ALTER COLUMN source_participation_id SET NOT NULL,
  ALTER COLUMN challenge_id SET NOT NULL,
  ALTER COLUMN evidence_key SET NOT NULL,
  ALTER COLUMN observed_at_utc SET NOT NULL,
  ALTER COLUMN evidence_payload SET NOT NULL,
  ALTER COLUMN evidence_version SET NOT NULL,
  ALTER COLUMN evidence_version SET DEFAULT 1;

CREATE UNIQUE INDEX IF NOT EXISTS ux_submissions_cheat_provenance
  ON "Submissions"(id, game_id, team_id, participation_id, challenge_id);
CREATE UNIQUE INDEX IF NOT EXISTS ux_cheatinfo_submission_id
  ON "CheatInfo"(submission_id);
CREATE INDEX IF NOT EXISTS ix_cheatinfo_game_observed
  ON "CheatInfo"(game_id, observed_at_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_cheatinfo_observed
  ON "CheatInfo"(observed_at_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_submissions_submit_remote_ip_hash
  ON "Submissions"(submit_remote_ip_hash, submit_time_utc, id)
  WHERE submit_remote_ip_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_submissions_container_id
  ON "Submissions"(container_id, submit_time_utc, id)
  WHERE container_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_submissions_wrong_game_part_challenge_time
  ON "Submissions"
     (game_id, participation_id, challenge_id, submit_time_utc, id)
  WHERE status = 2;
CREATE INDEX IF NOT EXISTS ix_submissions_game_time
  ON "Submissions"(game_id, submit_time_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_participations_competitive_cohort
  ON "Participations"(game_id, competitive_admitted_at_utc, id)
  WHERE competitive_admitted_at_utc IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_gameevents_submit_interactions
  ON "GameEvents"(game_id, team_id, (("values" ->> 0)), "Type",
                  publish_time_utc, id)
  WHERE "Type" IN (1, 5, 6);
CREATE INDEX IF NOT EXISTS ix_containeraccess_remote_ip_hash
  ON "ContainerAccessEvents"(remote_ip_hash, connected_at_utc, id)
  WHERE remote_ip_hash IS NOT NULL;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'fk_cheatinfo_submission_provenance'
       AND conrelid = '"CheatInfo"'::regclass
  ) THEN
    ALTER TABLE "CheatInfo"
      ADD CONSTRAINT fk_cheatinfo_submission_provenance
      FOREIGN KEY (submission_id, game_id, submit_team_id,
                   submit_participation_id, challenge_id)
      REFERENCES "Submissions"(id, game_id, team_id,
                                participation_id, challenge_id)
      ON DELETE RESTRICT;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'fk_cheatinfo_source_participation'
       AND conrelid = '"CheatInfo"'::regclass
  ) THEN
    ALTER TABLE "CheatInfo"
      ADD CONSTRAINT fk_cheatinfo_source_participation
      FOREIGN KEY (game_id, source_team_id, source_participation_id)
      REFERENCES "Participations"(game_id, team_id, id)
      ON DELETE RESTRICT;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'ck_cheatinfo_distinct_participations'
       AND conrelid = '"CheatInfo"'::regclass
  ) THEN
    ALTER TABLE "CheatInfo"
      ADD CONSTRAINT ck_cheatinfo_distinct_participations
      CHECK (submit_participation_id <> source_participation_id
         AND submit_team_id <> source_team_id);
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'ck_cheatinfo_evidence_contract'
       AND conrelid = '"CheatInfo"'::regclass
  ) THEN
    ALTER TABLE "CheatInfo"
      ADD CONSTRAINT ck_cheatinfo_evidence_contract
      CHECK (evidence_version = 1
         AND evidence_key = 'submission:' || submission_id::text
         AND jsonb_typeof(evidence_payload) = 'object');
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'ck_containeraccess_remote_ip_hash'
       AND conrelid = '"ContainerAccessEvents"'::regclass
  ) THEN
    ALTER TABLE "ContainerAccessEvents"
      ADD CONSTRAINT ck_containeraccess_remote_ip_hash
      CHECK (remote_ip_hash IS NULL OR OCTET_LENGTH(remote_ip_hash) = 32);
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'ck_submissions_immutable_observations'
       AND conrelid = '"Submissions"'::regclass
  ) THEN
    ALTER TABLE "Submissions"
      ADD CONSTRAINT ck_submissions_immutable_observations
      CHECK (
        (submit_remote_ip_hash IS NULL OR OCTET_LENGTH(submit_remote_ip_hash) = 32)
        AND (
          (container_last_operation_at_submit IS NULL
           AND container_was_loaded_at_submit IS NULL)
          OR
          (container_last_operation_at_submit IS NOT NULL
           AND container_was_loaded_at_submit IS NOT NULL)
        )
        AND (first_open_at_submit IS NULL
             OR first_open_at_submit <= submit_time_utc)
        AND (first_download_at_submit IS NULL
             OR first_download_at_submit <= submit_time_utc)
        AND (first_container_start_at_submit IS NULL
             OR first_container_start_at_submit <= submit_time_utc)
      );
  END IF;
END
$$;

CREATE OR REPLACE FUNCTION rsctf_validate_cheat_info_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
  canonical_game_id INTEGER;
  canonical_submit_team_id INTEGER;
  canonical_submit_participation_id INTEGER;
  canonical_challenge_id INTEGER;
  canonical_time TIMESTAMPTZ;
  canonical_status SMALLINT;
  canonical_answer TEXT;
  canonical_challenge_title TEXT;
  canonical_submit_team_name TEXT;
  canonical_submit_user_name TEXT;
  canonical_source_participation_id INTEGER;
  canonical_source_team_name TEXT;
BEGIN
  SELECT submission.game_id, submission.team_id,
         submission.participation_id, submission.challenge_id,
         submission.submit_time_utc, submission.status, submission.answer,
         challenge.title, submit_team.name,
         COALESCE((
           SELECT account.user_name
             FROM "AspNetUsers" account
            WHERE account.id = submission.user_id
            FOR SHARE
         ), '')
    INTO canonical_game_id, canonical_submit_team_id,
         canonical_submit_participation_id, canonical_challenge_id,
         canonical_time, canonical_status, canonical_answer,
         canonical_challenge_title, canonical_submit_team_name,
         canonical_submit_user_name
    FROM "Submissions" submission
    JOIN "GameChallenges" challenge ON challenge.id = submission.challenge_id
    JOIN "Teams" submit_team ON submit_team.id = submission.team_id
   WHERE submission.id = NEW.submission_id
   FOR SHARE OF submission, challenge, submit_team;
  IF NOT FOUND OR canonical_status <> 3 THEN
    RAISE EXCEPTION
      'CheatInfo must reference a canonical CheatDetected submission';
  END IF;
  IF NEW.game_id IS DISTINCT FROM canonical_game_id
     OR NEW.submit_team_id IS DISTINCT FROM canonical_submit_team_id THEN
    RAISE EXCEPTION 'CheatInfo submit identity does not match its submission';
  END IF;

  NEW.submit_participation_id := COALESCE(
    NEW.submit_participation_id, canonical_submit_participation_id
  );
  NEW.challenge_id := COALESCE(NEW.challenge_id, canonical_challenge_id);
  NEW.observed_at_utc := COALESCE(NEW.observed_at_utc, canonical_time);
  NEW.evidence_key := COALESCE(
    NEW.evidence_key, 'submission:' || NEW.submission_id::text
  );
  NEW.evidence_version := COALESCE(NEW.evidence_version, 1);
  IF NEW.submit_participation_id IS DISTINCT FROM canonical_submit_participation_id
     OR NEW.challenge_id IS DISTINCT FROM canonical_challenge_id
     OR NEW.observed_at_utc IS DISTINCT FROM canonical_time
     OR NEW.evidence_key IS DISTINCT FROM
          'submission:' || NEW.submission_id::text
     OR NEW.evidence_version <> 1 THEN
    RAISE EXCEPTION 'CheatInfo provenance does not match its submission';
  END IF;

  IF NEW.source_participation_id IS NULL THEN
    SELECT source.id, source_team.name
      INTO canonical_source_participation_id, canonical_source_team_name
      FROM "Participations" source
      JOIN "Teams" source_team ON source_team.id = source.team_id
      LEFT JOIN "GameInstances" instance
        ON instance.participation_id = source.id
       AND instance.challenge_id = canonical_challenge_id
      LEFT JOIN "FlagContexts" flag ON flag.id = instance.flag_id
     WHERE source.game_id = canonical_game_id
       AND source.team_id = NEW.source_team_id
     ORDER BY (flag.flag = canonical_answer) DESC NULLS LAST, source.id
     LIMIT 1
     FOR SHARE OF source, source_team;
    IF NOT FOUND THEN
      RAISE EXCEPTION 'CheatInfo source participation cannot be canonicalized';
    END IF;
    NEW.source_participation_id := canonical_source_participation_id;
  ELSE
    SELECT source_team.name
      INTO canonical_source_team_name
      FROM "Participations" source
      JOIN "Teams" source_team ON source_team.id = source.team_id
     WHERE source.id = NEW.source_participation_id
       AND source.game_id = canonical_game_id
       AND source.team_id = NEW.source_team_id
     FOR SHARE OF source, source_team;
    IF NOT FOUND THEN
      RAISE EXCEPTION 'CheatInfo source identity does not match its submission';
    END IF;
  END IF;

  -- Never trust a caller-supplied display snapshot. Construct the complete v1
  -- object from the locked canonical rows in this insertion transaction.
  NEW.evidence_payload := jsonb_build_object(
    'challengeTitle', canonical_challenge_title,
    'submitTeamName', canonical_submit_team_name,
    'sourceTeamName', canonical_source_team_name,
    'submitUserName', canonical_submit_user_name
  );
  RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION rsctf_reject_cheat_info_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  RAISE EXCEPTION 'CheatInfo is an immutable evidence ledger';
END
$$;

DROP TRIGGER IF EXISTS trg_cheatinfo_validate_insert ON "CheatInfo";
CREATE TRIGGER trg_cheatinfo_validate_insert
BEFORE INSERT ON "CheatInfo"
FOR EACH ROW EXECUTE FUNCTION rsctf_validate_cheat_info_insert();

DROP TRIGGER IF EXISTS trg_cheatinfo_immutable ON "CheatInfo";
CREATE TRIGGER trg_cheatinfo_immutable
BEFORE UPDATE OR DELETE ON "CheatInfo"
FOR EACH ROW EXECUTE FUNCTION rsctf_reject_cheat_info_mutation();

CREATE OR REPLACE FUNCTION rsctf_reject_submission_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'submission evidence is immutable';
  END IF;
  IF NEW.id IS DISTINCT FROM OLD.id
     OR NEW.answer IS DISTINCT FROM OLD.answer
     OR NEW.status IS DISTINCT FROM OLD.status
     OR NEW.submit_time_utc IS DISTINCT FROM OLD.submit_time_utc
     OR NEW.user_id IS DISTINCT FROM OLD.user_id
     OR NEW.team_id IS DISTINCT FROM OLD.team_id
     OR NEW.participation_id IS DISTINCT FROM OLD.participation_id
     OR NEW.game_id IS DISTINCT FROM OLD.game_id
     OR NEW.challenge_id IS DISTINCT FROM OLD.challenge_id
     OR NEW.submit_remote_ip_hash IS DISTINCT FROM OLD.submit_remote_ip_hash
     OR NEW.container_id IS DISTINCT FROM OLD.container_id
     OR NEW.container_last_operation_at_submit
          IS DISTINCT FROM OLD.container_last_operation_at_submit
     OR NEW.container_was_loaded_at_submit
          IS DISTINCT FROM OLD.container_was_loaded_at_submit
     OR NEW.first_open_at_submit IS DISTINCT FROM OLD.first_open_at_submit
     OR NEW.first_download_at_submit IS DISTINCT FROM OLD.first_download_at_submit
     OR NEW.first_container_start_at_submit
          IS DISTINCT FROM OLD.first_container_start_at_submit THEN
    RAISE EXCEPTION 'submission evidence is immutable';
  END IF;
  RETURN NEW;
END
$$;

CREATE TRIGGER trg_submission_core_immutable_update
BEFORE UPDATE ON "Submissions"
FOR EACH ROW EXECUTE FUNCTION rsctf_reject_submission_mutation();
CREATE TRIGGER trg_submission_core_immutable_delete
BEFORE DELETE ON "Submissions"
FOR EACH ROW EXECUTE FUNCTION rsctf_reject_submission_mutation();

DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM "FirstSolves" solve
    LEFT JOIN "Submissions" submission ON submission.id = solve.submission_id
    LEFT JOIN "Participations" participation ON participation.id = solve.participation_id
    LEFT JOIN "GameChallenges" challenge ON challenge.id = solve.challenge_id
    WHERE submission.id IS NULL OR submission.status <> 1
       OR submission.participation_id IS DISTINCT FROM solve.participation_id
       OR submission.challenge_id IS DISTINCT FROM solve.challenge_id
       OR participation.game_id IS DISTINCT FROM submission.game_id
       OR challenge.game_id IS DISTINCT FROM submission.game_id
  ) THEN
    RAISE EXCEPTION 'cannot freeze malformed FirstSolves provenance';
  END IF;
END
$$;
CREATE OR REPLACE FUNCTION rsctf_validate_firstsolve_insert()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM 1 FROM "Submissions" submission
  JOIN "Participations" participation ON participation.id = NEW.participation_id
  JOIN "GameChallenges" challenge ON challenge.id = NEW.challenge_id
  WHERE submission.id = NEW.submission_id AND submission.status = 1
    AND submission.participation_id = NEW.participation_id
    AND submission.challenge_id = NEW.challenge_id
    AND participation.game_id = submission.game_id
    AND challenge.game_id = submission.game_id
  FOR SHARE OF submission, participation, challenge;
  IF NOT FOUND THEN RAISE EXCEPTION 'FirstSolves requires an Accepted submission tuple'; END IF;
  RETURN NEW;
END
$$;
CREATE OR REPLACE FUNCTION rsctf_reject_evidence_mutation()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'evidence rows are append-only';
END
$$;
CREATE TRIGGER trg_firstsolve_validate_insert BEFORE INSERT ON "FirstSolves"
FOR EACH ROW EXECUTE FUNCTION rsctf_validate_firstsolve_insert();
CREATE TRIGGER trg_firstsolve_immutable BEFORE UPDATE OR DELETE ON "FirstSolves"
FOR EACH ROW EXECUTE FUNCTION rsctf_reject_evidence_mutation();
CREATE TRIGGER trg_containeraccess_immutable
BEFORE UPDATE OR DELETE ON "ContainerAccessEvents"
FOR EACH ROW EXECUTE FUNCTION rsctf_reject_evidence_mutation();

-- Defense in depth for a writer that was already in flight at the cutover
-- boundary. Populate fields an older submit binary cannot name, while
-- preserving NULL for the unavailable legacy request-IP identity.
CREATE OR REPLACE FUNCTION rsctf_snapshot_legacy_submission_observations()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
  snapshot_container_id UUID;
  snapshot_last_operation TIMESTAMPTZ;
  snapshot_was_loaded BOOLEAN;
  snapshot_first_open TIMESTAMPTZ;
  snapshot_first_download TIMESTAMPTZ;
  snapshot_first_container_start TIMESTAMPTZ;
BEGIN
  IF NEW.container_id IS NULL
     AND NEW.container_last_operation_at_submit IS NULL
     AND NEW.container_was_loaded_at_submit IS NULL THEN
    SELECT COALESCE(instance.container_id, challenge.shared_container_id),
           instance.last_container_operation, instance.is_loaded
      INTO snapshot_container_id, snapshot_last_operation,
           snapshot_was_loaded
      FROM "GameChallenges" challenge
      LEFT JOIN "GameInstances" instance
        ON instance.challenge_id = challenge.id
       AND instance.participation_id = NEW.participation_id
     WHERE challenge.id = NEW.challenge_id
       AND challenge.game_id = NEW.game_id;
    IF FOUND THEN
      NEW.container_id := snapshot_container_id;
      NEW.container_last_operation_at_submit := snapshot_last_operation;
      NEW.container_was_loaded_at_submit := snapshot_was_loaded;
    END IF;
  END IF;
  IF NEW.first_open_at_submit IS NULL
     AND NEW.first_download_at_submit IS NULL
     AND NEW.first_container_start_at_submit IS NULL THEN
    SELECT MIN(event.publish_time_utc) FILTER (WHERE event."Type" = 6),
           MIN(event.publish_time_utc) FILTER (WHERE event."Type" = 5),
           MIN(event.publish_time_utc) FILTER (WHERE event."Type" = 1)
      INTO snapshot_first_open, snapshot_first_download,
           snapshot_first_container_start
      FROM "GameEvents" event
      JOIN "Games" game ON game.id = event.game_id
     WHERE event.game_id = NEW.game_id
       AND event.team_id = NEW.team_id
       AND event."values" ->> 0 = NEW.challenge_id::text
       AND event.publish_time_utc >= game.start_time_utc
       AND event.publish_time_utc < game.end_time_utc
       AND event.publish_time_utc <= NEW.submit_time_utc
       AND event."Type" IN (1, 5, 6);
    NEW.first_open_at_submit := snapshot_first_open;
    NEW.first_download_at_submit := snapshot_first_download;
    NEW.first_container_start_at_submit := snapshot_first_container_start;
  END IF;
  RETURN NEW;
END
$$;

CREATE TRIGGER trg_submissions_snapshot_observations
BEFORE INSERT ON "Submissions"
FOR EACH ROW EXECUTE FUNCTION rsctf_snapshot_legacy_submission_observations();

CREATE OR REPLACE FUNCTION rsctf_capture_competitive_admission()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
  competitive_end TIMESTAMPTZ;
  admitted_at TIMESTAMPTZ;
  evidence_open BOOLEAN;
BEGIN
  IF TG_OP = 'INSERT' THEN
    IF NEW.competitive_admitted_at_utc IS NOT NULL THEN
      RAISE EXCEPTION 'competitive admission time is database-assigned';
    END IF;
  ELSE
    IF NEW.competitive_admitted_at_utc
         IS DISTINCT FROM OLD.competitive_admitted_at_utc THEN
      RAISE EXCEPTION 'competitive admission time is immutable';
    END IF;
    IF OLD.competitive_admitted_at_utc IS NOT NULL THEN
      IF NEW.game_id IS DISTINCT FROM OLD.game_id THEN
        RAISE EXCEPTION 'competitive participation game is immutable';
      END IF;
      RETURN NEW;
    END IF;
  END IF;

  IF NEW.status IN (1, 3) THEN
    SELECT end_time_utc
      INTO competitive_end
      FROM "Games"
     WHERE id = NEW.game_id
     FOR SHARE;
    IF FOUND THEN
      -- Every game has a durable state row. Locking it makes a participation
      -- INSERT that began before final closure observe the just-committed
      -- updated tuple after it resumes behind Games FOR SHARE.
      SELECT reconciliation.evidence_closed_at_utc IS NULL
        INTO evidence_open
        FROM "SuspicionReconciliationState" reconciliation
       WHERE reconciliation.game_id = NEW.game_id
       FOR SHARE;
      admitted_at := clock_timestamp();
      IF evidence_open AND admitted_at < competitive_end THEN
        NEW.competitive_admitted_at_utc := admitted_at;
      END IF;
    END IF;
  END IF;
  RETURN NEW;
END
$$;

CREATE TRIGGER trg_participations_competitive_admission
BEFORE INSERT OR UPDATE OF status, game_id, competitive_admitted_at_utc
ON "Participations"
FOR EACH ROW EXECUTE FUNCTION rsctf_capture_competitive_admission();

CREATE TABLE IF NOT EXISTS "SuspicionEvaluationOutbox" (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  job_kind SMALLINT NOT NULL,
  source_kind SMALLINT NOT NULL,
  source_id INTEGER NOT NULL,
  game_id INTEGER NOT NULL,
  participation_id INTEGER NOT NULL,
  challenge_id INTEGER,
  rule_kind SMALLINT,
  evidence_key TEXT NOT NULL,
  observed_at_utc TIMESTAMPTZ NOT NULL,
  evidence_payload JSONB NOT NULL DEFAULT '{}'::jsonb,
  evidence_version SMALLINT NOT NULL DEFAULT 1,
  available_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  lease_token UUID,
  lease_expires_at_utc TIMESTAMPTZ,
  attempts INTEGER NOT NULL DEFAULT 0,
  completed_at_utc TIMESTAMPTZ,
  last_error TEXT,
  CONSTRAINT fk_suspicion_outbox_game
    FOREIGN KEY (game_id) REFERENCES "Games"(id) ON DELETE RESTRICT,
  CONSTRAINT fk_suspicion_outbox_participation
    FOREIGN KEY (game_id, participation_id)
    REFERENCES "Participations"(game_id, id) ON DELETE RESTRICT,
  CONSTRAINT fk_suspicion_outbox_challenge
    FOREIGN KEY (game_id, challenge_id)
    REFERENCES "GameChallenges"(game_id, id) ON DELETE RESTRICT,
  CONSTRAINT ck_suspicion_outbox_kind CHECK (
    (job_kind = 0 AND source_kind = 0 AND rule_kind IS NULL
                  AND challenge_id IS NOT NULL)
    OR
    (job_kind = 1 AND source_kind = 2 AND rule_kind = 33
                  AND challenge_id IS NOT NULL)
  ),
  CONSTRAINT ck_suspicion_outbox_payload CHECK (
    evidence_version = 1
    AND jsonb_typeof(evidence_payload) = 'object'
    AND OCTET_LENGTH(evidence_key) BETWEEN 1 AND 128
  ),
  CONSTRAINT ck_suspicion_outbox_lease CHECK (
    (lease_token IS NULL) = (lease_expires_at_utc IS NULL)
  ),
  CONSTRAINT ck_suspicion_outbox_attempts CHECK (attempts >= 0)
);
-- Reconcile a partial/idempotent rerun to the final narrow direct-source
-- contract. Global HTTP/TCP honeypots are raw telemetry and never score jobs.
ALTER TABLE "SuspicionEvaluationOutbox"
  DROP CONSTRAINT IF EXISTS ck_suspicion_outbox_kind;
ALTER TABLE "SuspicionEvaluationOutbox"
  ADD CONSTRAINT ck_suspicion_outbox_kind CHECK (
    (job_kind = 0 AND source_kind = 0 AND rule_kind IS NULL
                  AND challenge_id IS NOT NULL)
    OR
    (job_kind = 1 AND source_kind = 2 AND rule_kind = 33
                  AND challenge_id IS NOT NULL)
  );

-- Evaluation identity is immutable evidence. Workers may mutate only lease,
-- retry, and completion bookkeeping; deleting a job could silently erase a
-- failed detector attempt or make a game appear fully reconciled.
CREATE OR REPLACE FUNCTION rsctf_guard_outbox_operational_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'suspicion evaluation jobs cannot be deleted';
  END IF;
  IF NEW.id IS DISTINCT FROM OLD.id
     OR NEW.job_kind IS DISTINCT FROM OLD.job_kind
     OR NEW.source_kind IS DISTINCT FROM OLD.source_kind
     OR NEW.source_id IS DISTINCT FROM OLD.source_id
     OR NEW.game_id IS DISTINCT FROM OLD.game_id
     OR NEW.participation_id IS DISTINCT FROM OLD.participation_id
     OR NEW.challenge_id IS DISTINCT FROM OLD.challenge_id
     OR NEW.rule_kind IS DISTINCT FROM OLD.rule_kind
     OR NEW.evidence_key IS DISTINCT FROM OLD.evidence_key
     OR NEW.observed_at_utc IS DISTINCT FROM OLD.observed_at_utc
     OR NEW.evidence_payload IS DISTINCT FROM OLD.evidence_payload
     OR NEW.evidence_version IS DISTINCT FROM OLD.evidence_version THEN
    RAISE EXCEPTION 'suspicion evaluation identity is immutable';
  END IF;
  RETURN NEW;
END
$$;
DROP TRIGGER IF EXISTS trg_outbox_operational_update
  ON "SuspicionEvaluationOutbox";
CREATE TRIGGER trg_outbox_operational_update
BEFORE UPDATE OR DELETE ON "SuspicionEvaluationOutbox"
FOR EACH ROW EXECUTE FUNCTION rsctf_guard_outbox_operational_update();

CREATE UNIQUE INDEX IF NOT EXISTS ux_suspicion_outbox_source
  ON "SuspicionEvaluationOutbox"
     (source_kind, source_id, COALESCE(rule_kind, -1), evidence_key);
CREATE INDEX IF NOT EXISTS ix_suspicion_outbox_pending
  ON "SuspicionEvaluationOutbox"(available_at_utc, id)
  WHERE completed_at_utc IS NULL;
CREATE INDEX IF NOT EXISTS ix_suspicion_outbox_game_observed
  ON "SuspicionEvaluationOutbox"(game_id, observed_at_utc DESC, id DESC);

-- m0091 quarantines every event created by pre-cutover mutable detectors. Seed
-- durable re-evaluation from the canonical hard evidence while this migration
-- owns an exclusive writer fence, so a valid stolen-flag incident is restored
-- exactly once after all migrations complete.
INSERT INTO "SuspicionEvaluationOutbox"
    (job_kind, source_kind, source_id, game_id, participation_id,
     challenge_id, rule_kind, evidence_key, observed_at_utc,
     evidence_payload, evidence_version)
SELECT 0, 0, submission.id, submission.game_id,
       submission.participation_id, submission.challenge_id, NULL,
       'submission:' || submission.id::text, submission.submit_time_utc,
       '{}'::jsonb, 1
  FROM "CheatInfo" cheat
  JOIN "Submissions" submission
    ON submission.id = cheat.submission_id
   AND submission.game_id = cheat.game_id
   AND submission.participation_id = cheat.submit_participation_id
   AND submission.challenge_id = cheat.challenge_id
  JOIN "Games" game ON game.id = submission.game_id
 WHERE submission.status = 3
   AND submission.submit_time_utc >= game.start_time_utc
   AND submission.submit_time_utc < game.end_time_utc
ON CONFLICT DO NOTHING;

-- The trigger is the cross-version transactional hand-off. New writers verify
-- this exact row before commit; old writers gain durable evaluation without a
-- best-effort post-commit gap.
CREATE OR REPLACE FUNCTION rsctf_enqueue_submission_evaluation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO "SuspicionEvaluationOutbox"
      (job_kind, source_kind, source_id, game_id, participation_id,
       challenge_id, rule_kind, evidence_key, observed_at_utc,
       evidence_payload, evidence_version)
  VALUES
      (0, 0, NEW.id, NEW.game_id, NEW.participation_id,
       NEW.challenge_id, NULL, 'submission:' || NEW.id::text,
       NEW.submit_time_utc, '{}'::jsonb, 1)
  ON CONFLICT DO NOTHING;
  RETURN NEW;
END
$$;

CREATE TRIGGER trg_submissions_evaluation_outbox
AFTER INSERT ON "Submissions"
FOR EACH ROW EXECUTE FUNCTION rsctf_enqueue_submission_evaluation();

CREATE TABLE IF NOT EXISTS "SuspicionReconciliationState" (
  game_id INTEGER PRIMARY KEY
    REFERENCES "Games"(id) ON DELETE CASCADE,
  evidence_closed_at_utc TIMESTAMPTZ,
  last_reconciled_at_utc TIMESTAMPTZ,
  sealed_at_utc TIMESTAMPTZ,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  CONSTRAINT ck_suspicion_reconciliation_attempts CHECK (attempts >= 0),
  CONSTRAINT ck_suspicion_reconciliation_seal CHECK (
    sealed_at_utc IS NULL OR evidence_closed_at_utc IS NOT NULL
  )
);
CREATE OR REPLACE FUNCTION rsctf_seed_suspicion_reconciliation_state()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO "SuspicionReconciliationState" (game_id, attempts)
  VALUES (NEW.id, 0)
  ON CONFLICT (game_id) DO NOTHING;
  RETURN NEW;
END
$$;
CREATE TRIGGER trg_games_seed_suspicion_reconciliation
AFTER INSERT ON "Games"
FOR EACH ROW EXECUTE FUNCTION rsctf_seed_suspicion_reconciliation_state();
INSERT INTO "SuspicionReconciliationState" (game_id, attempts)
SELECT game.id, 0 FROM "Games" game
ON CONFLICT (game_id) DO NOTHING;
ALTER TABLE "SuspicionReconciliationState"
  ADD COLUMN IF NOT EXISTS evidence_closed_at_utc TIMESTAMPTZ;
UPDATE "SuspicionReconciliationState"
   SET evidence_closed_at_utc = sealed_at_utc
 WHERE sealed_at_utc IS NOT NULL
   AND evidence_closed_at_utc IS NULL;
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'ck_suspicion_reconciliation_seal'
       AND conrelid = '"SuspicionReconciliationState"'::regclass
  ) THEN
    ALTER TABLE "SuspicionReconciliationState"
      ADD CONSTRAINT ck_suspicion_reconciliation_seal CHECK (
        sealed_at_utc IS NULL OR evidence_closed_at_utc IS NOT NULL
      );
  END IF;
END
$$;
CREATE INDEX IF NOT EXISTS ix_suspicion_reconciliation_unsealed
  ON "SuspicionReconciliationState"(game_id)
  WHERE sealed_at_utc IS NULL;

-- Historical anti-cheat reconciliation reads these relations in attribution
-- and event-time order. Keep the outbox worker's bounded passes index-only at
-- the selection edge instead of repeatedly scanning an event's full history.
CREATE INDEX IF NOT EXISTS ix_honeypothits_game_participation_time
  ON "HoneypotHits"(game_id, participation_id, hit_at_utc, id);
CREATE INDEX IF NOT EXISTS ix_honeypothits_game_time
  ON "HoneypotHits"(game_id, hit_at_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_containeraccess_challenge_owner_container_time
  ON "ContainerAccessEvents"
     (challenge_id, container_owner_participation_id, container_id, connected_at_utc);
CREATE INDEX IF NOT EXISTS ix_containeraccess_game_time
  ON "ContainerAccessEvents"(game_id, connected_at_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_suspicionevents_created_id
  ON "SuspicionEvents"(created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_suspicionevents_participation
  ON "SuspicionEvents"(participation_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_suspicionevents_challenge
  ON "SuspicionEvents"(challenge_id, created_at DESC, id DESC)
  WHERE challenge_id IS NOT NULL;
"#;

const DOWN_SQL: &str = include_str!("m0089_cheat_evidence_ledger_down.sql");

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m0089_cheat_evidence_ledger_tests.rs"]
mod tests;
