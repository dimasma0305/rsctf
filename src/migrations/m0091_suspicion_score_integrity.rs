//! Freeze suspicion-event weights, constrain rule identities/weights, and
//! rebuild the cached participation score from the authoritative event ledger.
//!
//! Runtime writes use the same Rust `compute_breakdown` projection after this
//! one-time repair. The only permanent trigger makes event rows immutable; no
//! SQL scoring function or score-maintenance trigger is installed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
-- Drain every legacy writer at the same outer-to-inner fence used by detector
-- transactions, then prevent new writes until the frozen ledger and cached
-- projection agree. EXCLUSIVE still permits ordinary read-only reports.
LOCK TABLE "Games", "GameChallenges", "Participations", "SuspicionEvents",
           "SuspicionRules"
  IN EXCLUSIVE MODE;

-- A mismatched event would be counted by participation_id but hidden by the
-- game-scoped report join. Preserve the evidence and fail with a repairable
-- diagnostic instead of silently blessing or discarding its provenance.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM "SuspicionEvents" event
          LEFT JOIN "Participations" participation
            ON participation.game_id = event.game_id
           AND participation.id = event.participation_id
         WHERE participation.id IS NULL
    ) THEN
        RAISE EXCEPTION
          'cannot canonicalize SuspicionEvents with mismatched participation provenance'
          USING HINT = 'Repair event.game_id/participation_id attribution before retrying m0091.';
    END IF;

    IF EXISTS (
        SELECT 1
          FROM "SuspicionEvents" event
          LEFT JOIN "GameChallenges" challenge
            ON challenge.game_id = event.game_id
           AND challenge.id = event.challenge_id
         WHERE event.challenge_id IS NOT NULL
           AND challenge.id IS NULL
    ) THEN
        RAISE EXCEPTION
          'cannot canonicalize SuspicionEvents with mismatched challenge provenance'
          USING HINT = 'Repair event.game_id/challenge_id attribution before retrying m0091.';
    END IF;
END
$$;

-- Pre-cutover detectors mixed live, mutable, and non-windowed predicates. Their
-- rows are retained for forensic database audit, but cannot safely be treated
-- as canonical evidence or keep the canonical evidence key occupied. A durable
-- marker makes this one-time quarantine idempotent when operators rerun the
-- unshipped migration SQL during recovery.
CREATE TABLE IF NOT EXISTS "SuspicionLedgerCutoverState" (
    version SMALLINT PRIMARY KEY CHECK (version = 1),
    completed_at_utc TIMESTAMPTZ NOT NULL
);

WITH first_cutover AS (
    INSERT INTO "SuspicionLedgerCutoverState" (version, completed_at_utc)
    VALUES (1, clock_timestamp())
    ON CONFLICT (version) DO NOTHING
    RETURNING version
)
UPDATE "SuspicionEvents" event
   SET evidence_key = 'legacy-untrusted:' || event.id::text,
       score_delta = 0
 WHERE EXISTS (SELECT 1 FROM first_cutover);

WITH defaults(kind, rule_code, default_weight) AS (
    VALUES
        (0::smallint,  'StolenFlag',                       100),
        (1::smallint,  'SharedIP',                          10),
        (2::smallint,  'SharedFingerprint',                 60),
        (3::smallint,  'FingerprintChurn',                  30),
        (4::smallint,  'IpChurn',                           20),
        (5::smallint,  'UnknownIP',                         10),
        (6::smallint,  'CrossTeamIP',                       20),
        (7::smallint,  'TokenAbuse',                        80),
        (8::smallint,  'Hoarding',                          30),
        (9::smallint,  'Burst',                             30),
        (10::smallint, 'NoDownload',                        80),
        (11::smallint, 'NoContainer',                       80),
        (12::smallint, 'FastSolve-Open',                    50),
        (13::smallint, 'FastSolve-Download',                50),
        (14::smallint, 'FastSolve-Container',               50),
        (15::smallint, 'SequenceSimilarity',                40),
        (16::smallint, 'CollusionGroup',                    10),
        (17::smallint, 'ZeroWrongAttempts',                 50),
        (18::smallint, 'WrongFlagLeakage',                  80),
        (19::smallint, 'SolutionRelay',                     60),
        (20::smallint, 'AdaptiveFastSolve',                 60),
        (21::smallint, 'DirectedSolving',                   30),
        (22::smallint, 'ClusteredRegistration',             40),
        (23::smallint, 'SubnetOverlap',                      5),
        (24::smallint, 'HighWrongRate',                     40),
        (25::smallint, 'AutomatedPattern',                  50),
        (26::smallint, 'SessionConcurrency',                30),
        (27::smallint, 'FirstBloodAnomaly',                 20),
        (28::smallint, 'HoneypotHit',                       70),
        (29::smallint, 'HoneypotProtocolHit',               90),
        (30::smallint, 'HoneypotCanaryFlag',               100),
        (31::smallint, 'HoneypotChain',                    150),
        (32::smallint, 'FlagEgress',                        80),
        (33::smallint, 'CrossTeamContainerAccess',         120),
        (34::smallint, 'DelayedSolveSubmission',            40),
        (35::smallint, 'InstantSubmitAfterAccess',           50),
        (36::smallint, 'SubmitterNeverAccessedContainer',   30),
        (37::smallint, 'AccessIpMismatchAtSubmission',      30)
)
UPDATE "SuspicionEvents" event
   SET score_delta = COALESCE(rule.weight, defaults.default_weight)
  FROM defaults
  LEFT JOIN "SuspicionRules" rule ON rule.rule_code = defaults.rule_code
 WHERE event.kind = defaults.kind
   AND event.score_delta IS NULL;

ALTER TABLE "SuspicionEvents"
  ALTER COLUMN score_delta SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'ck_suspicionevents_kind'
           AND conrelid = '"SuspicionEvents"'::regclass
    ) THEN
        ALTER TABLE "SuspicionEvents"
          ADD CONSTRAINT ck_suspicionevents_kind
          CHECK (kind BETWEEN 0 AND 37);
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'ck_suspicionevents_score_delta'
           AND conrelid = '"SuspicionEvents"'::regclass
    ) THEN
        ALTER TABLE "SuspicionEvents"
          ADD CONSTRAINT ck_suspicionevents_score_delta
          CHECK (score_delta BETWEEN 0 AND 10000);
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'ck_suspicionrules_weight'
           AND conrelid = '"SuspicionRules"'::regclass
    ) THEN
        ALTER TABLE "SuspicionRules"
          ADD CONSTRAINT ck_suspicionrules_weight
          CHECK (weight BETWEEN 0 AND 10000);
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'fk_suspicionevents_participation_provenance'
           AND conrelid = '"SuspicionEvents"'::regclass
    ) THEN
        ALTER TABLE "SuspicionEvents"
          ADD CONSTRAINT fk_suspicionevents_participation_provenance
          FOREIGN KEY (game_id, participation_id)
          REFERENCES "Participations"(game_id, id)
          ON DELETE RESTRICT;
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'fk_suspicionevents_challenge_provenance'
           AND conrelid = '"SuspicionEvents"'::regclass
    ) THEN
        ALTER TABLE "SuspicionEvents"
          ADD CONSTRAINT fk_suspicionevents_challenge_provenance
          FOREIGN KEY (game_id, challenge_id)
          REFERENCES "GameChallenges"(game_id, id)
          ON DELETE RESTRICT;
    END IF;
END
$$;

WITH classified AS MATERIALIZED (
    SELECT event.id,
           event.participation_id,
           event.kind,
           event.score_delta,
           event.created_at,
           event.evidence_key LIKE 'legacy:%' AS is_legacy,
           event.evidence_key LIKE 'legacy-untrusted:%' AS is_untrusted,
           ROW_NUMBER() OVER (
               PARTITION BY event.participation_id,
                            event.kind,
                            (event.evidence_key LIKE 'legacy:%')
               ORDER BY event.created_at DESC, event.id DESC
           ) AS legacy_rank,
           CASE
               WHEN event.kind IN (0, 7, 18, 30, 33) THEN 'hard'
               WHEN event.kind IN (19, 24, 25) THEN 'strong'
               WHEN event.kind IN (1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 21, 22, 23, 26, 28, 29, 31, 32, 36, 37)
                   THEN 'context'
               ELSE 'behavioral'
           END AS tier,
           CASE event.kind
               WHEN 0 THEN 10
               WHEN 7 THEN 5
               WHEN 18 THEN 10
               WHEN 19 THEN 2
               WHEN 21 THEN 1
               WHEN 27 THEN 4
               WHEN 28 THEN 5
               WHEN 31 THEN 1
               WHEN 33 THEN 10
               WHEN 34 THEN 5
               WHEN 16 THEN 1
               ELSE 3
           END AS incident_cap,
           CASE event.kind
               WHEN 10 THEN 0
               WHEN 11 THEN 0
               WHEN 12 THEN 0
               WHEN 13 THEN 0
               WHEN 14 THEN 0
               WHEN 21 THEN 0
               WHEN 22 THEN 0
               WHEN 28 THEN 0
               WHEN 29 THEN 0
               WHEN 31 THEN 0
               WHEN 32 THEN 0
               WHEN 36 THEN 0
               WHEN 2 THEN 20
               WHEN 6 THEN 10
               WHEN 26 THEN 10
               ELSE 5
           END::bigint AS corroboration_unit
      FROM "SuspicionEvents" event
), deduplicated AS MATERIALIZED (
    SELECT *
      FROM classified
     WHERE NOT is_untrusted
       AND (NOT is_legacy OR legacy_rank = 1)
), ranked AS MATERIALIZED (
    SELECT deduplicated.*,
           ROW_NUMBER() OVER (
               PARTITION BY participation_id, kind
               ORDER BY created_at DESC, id DESC
           ) AS incident_rank
      FROM deduplicated
), rule_totals AS MATERIALIZED (
    SELECT participation_id,
           kind,
           tier,
           corroboration_unit,
           COALESCE(
               SUM(score_delta::bigint)
                   FILTER (WHERE incident_rank <= incident_cap),
               0
           )::bigint AS rule_score
      FROM ranked
     GROUP BY participation_id, kind, tier, corroboration_unit
), participant_totals AS MATERIALIZED (
    SELECT participation_id,
           COALESCE(SUM(rule_score) FILTER (WHERE tier = 'hard'), 0)::bigint AS hard,
           COALESCE(SUM(rule_score) FILTER (WHERE tier = 'strong'), 0)::bigint AS strong,
           COALESCE(SUM(rule_score) FILTER (WHERE tier = 'behavioral'), 0)::bigint
               AS behavioral,
           COALESCE(
               SUM(corroboration_unit) FILTER (WHERE tier = 'context'),
               0
           )::bigint AS context_units
      FROM rule_totals
     GROUP BY participation_id
), normalized AS MATERIALIZED (
    SELECT participation.id,
           COALESCE(total.hard, 0)::bigint AS hard,
           COALESCE(total.strong, 0)::bigint AS strong,
           COALESCE(total.behavioral, 0)::bigint AS behavioral,
           COALESCE(total.context_units, 0)::bigint AS context_units
      FROM "Participations" participation
      LEFT JOIN participant_totals total ON total.participation_id = participation.id
), canonical AS (
    SELECT id,
           LEAST(
               GREATEST(
                   hard
                   + LEAST(strong, 60::bigint)
                   + LEAST(behavioral, 25::bigint)
                   + CASE
                         WHEN hard > 0 THEN LEAST(hard / 2, context_units)
                         ELSE 0
                     END,
                   0::bigint
               ),
               2147483647::bigint
           )::integer AS score
      FROM normalized
)
UPDATE "Participations" participation
   SET suspicion_score = canonical.score
  FROM canonical
 WHERE participation.id = canonical.id
   AND participation.suspicion_score IS DISTINCT FROM canonical.score;

CREATE OR REPLACE FUNCTION rsctf_reject_suspicion_event_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'SuspicionEvents is an immutable evidence ledger';
END
$$;

DROP TRIGGER IF EXISTS trg_suspicionevents_immutable ON "SuspicionEvents";
CREATE TRIGGER trg_suspicionevents_immutable
BEFORE UPDATE OR DELETE ON "SuspicionEvents"
FOR EACH ROW EXECUTE FUNCTION rsctf_reject_suspicion_event_mutation();
"#;

const DOWN_SQL: &str = r#"
DROP TRIGGER IF EXISTS trg_suspicionevents_immutable ON "SuspicionEvents";
DROP FUNCTION IF EXISTS rsctf_reject_suspicion_event_mutation();
DROP TABLE IF EXISTS "SuspicionLedgerCutoverState";
ALTER TABLE "SuspicionRules"
  DROP CONSTRAINT IF EXISTS ck_suspicionrules_weight;
ALTER TABLE "SuspicionEvents"
  DROP CONSTRAINT IF EXISTS fk_suspicionevents_challenge_provenance,
  DROP CONSTRAINT IF EXISTS fk_suspicionevents_participation_provenance,
  DROP CONSTRAINT IF EXISTS ck_suspicionevents_score_delta,
  DROP CONSTRAINT IF EXISTS ck_suspicionevents_kind,
  ALTER COLUMN score_delta DROP NOT NULL;
"#;

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
mod tests {
    use super::UP_SQL;
    use crate::services::suspicion::{SuspicionType, DEFAULTS};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    async fn isolated_schema(prefix: &str) -> (sqlx::PgPool, sqlx::PgPool, String) {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect PostgreSQL migration test admin");
        let schema = format!("{prefix}_{}", uuid::Uuid::new_v4().simple());
        assert!(schema
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric()));
        sqlx::raw_sql(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated m0091 schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse PostgreSQL migration test URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .expect("connect isolated m0091 schema");
        (admin, pool, schema)
    }

    async fn create_pre_m0091_schema(pool: &sqlx::PgPool) {
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                UNIQUE (game_id, id)
            );
            CREATE TABLE "Participations" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL,
                suspicion_score INTEGER NOT NULL DEFAULT 0,
                UNIQUE (game_id, id)
            );
            CREATE TABLE "SuspicionRules" (
                id SERIAL PRIMARY KEY,
                rule_code TEXT NOT NULL UNIQUE,
                weight INTEGER NOT NULL,
                description TEXT NOT NULL
            );
            CREATE TABLE "SuspicionEvents" (
                id SERIAL PRIMARY KEY,
                game_id INTEGER NOT NULL,
                participation_id INTEGER NOT NULL,
                challenge_id INTEGER,
                kind SMALLINT NOT NULL,
                evidence_key TEXT NOT NULL,
                score_delta INTEGER,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                UNIQUE (game_id, participation_id, kind, evidence_key)
            );
            "#,
        )
        .execute(pool)
        .await
        .expect("create pre-m0091 tables");
    }

    async fn drop_isolated_schema(admin: sqlx::PgPool, pool: sqlx::PgPool, schema: String) {
        pool.close().await;
        assert!(schema.starts_with("m0091_"));
        sqlx::raw_sql(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated m0091 schema");
        admin.close().await;
    }

    #[test]
    fn migration_preserves_all_38_historical_kind_mappings() {
        let mappings = UP_SQL
            .lines()
            .filter(|line| line.contains("::smallint"))
            .map(|line| {
                let fields = line
                    .trim()
                    .trim_start_matches('(')
                    .trim_end_matches(',')
                    .trim_end_matches(')')
                    .split(',')
                    .map(str::trim)
                    .collect::<Vec<_>>();
                let kind = fields[0]
                    .trim_end_matches("::smallint")
                    .parse::<i16>()
                    .expect("migration kind is an i16");
                let code = fields[1].trim_matches('\'').to_string();
                let weight = fields[2]
                    .parse::<i32>()
                    .expect("migration weight is an i32");
                (kind, code, weight)
            })
            .collect::<Vec<_>>();

        assert_eq!(mappings.len(), 38);
        for (position, ((kind, code, weight), (ty, expected_weight, _))) in
            mappings.iter().zip(DEFAULTS).enumerate()
        {
            assert_eq!(*kind, i16::try_from(position).unwrap());
            assert_eq!(SuspicionType::from_kind(*kind), Some(*ty));
            assert_eq!(code, ty.code());
            assert_eq!(*weight, *expected_weight);
        }
    }

    #[test]
    fn migration_freezes_and_bounds_the_authoritative_ledger() {
        assert!(UP_SQL.contains(
            "LOCK TABLE \"Games\", \"GameChallenges\", \"Participations\", \"SuspicionEvents\""
        ));
        assert!(UP_SQL.contains("IN EXCLUSIVE MODE"));
        assert!(UP_SQL.contains("ALTER COLUMN score_delta SET NOT NULL"));
        assert!(UP_SQL.contains("CHECK (kind BETWEEN 0 AND 37)"));
        assert!(UP_SQL.contains("CHECK (weight BETWEEN 0 AND 10000)"));
        assert!(UP_SQL.contains("CHECK (score_delta BETWEEN 0 AND 10000)"));
        assert!(UP_SQL.contains("SUM(score_delta::bigint)"));
        assert!(UP_SQL.contains("mismatched participation provenance"));
        assert!(UP_SQL.contains("mismatched challenge provenance"));
        assert!(UP_SQL.contains("fk_suspicionevents_participation_provenance"));
        assert!(UP_SQL.contains("fk_suspicionevents_challenge_provenance"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"SuspicionLedgerCutoverState\""));
        assert!(UP_SQL.contains("'legacy-untrusted:' || event.id::text"));
        assert!(UP_SQL.contains("WHERE NOT is_untrusted"));
        assert!(UP_SQL.contains("trg_suspicionevents_immutable"));
    }

    #[test]
    fn one_time_backfill_applies_caps_and_zero_direct_context_score() {
        assert!(UP_SQL.contains("incident_rank <= incident_cap"));
        assert!(UP_SQL.contains("LEAST(strong, 60::bigint)"));
        assert!(UP_SQL.contains("LEAST(behavioral, 25::bigint)"));
        assert!(UP_SQL.contains("WHEN hard > 0 THEN LEAST(hard / 2, context_units)"));
        assert!(UP_SQL.contains("WHEN 10 THEN 0"));
        assert!(UP_SQL.contains("WHEN 11 THEN 0"));
        assert!(UP_SQL.contains("WHEN 12 THEN 0"));
        assert!(UP_SQL.contains("WHEN 13 THEN 0"));
        assert!(UP_SQL.contains("WHEN 14 THEN 0"));
        assert!(UP_SQL.contains("WHEN 21 THEN 0"));
        assert!(UP_SQL.contains("WHEN 22 THEN 0"));
        assert!(UP_SQL.contains("WHEN 28 THEN 0"));
        assert!(UP_SQL.contains("WHEN 29 THEN 0"));
        assert!(UP_SQL.contains("WHEN 31 THEN 0"));
        assert!(UP_SQL.contains("WHEN 32 THEN 0"));
        assert!(UP_SQL.contains("WHEN 36 THEN 0"));
        assert!(!UP_SQL.contains("+ context_units\n"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_drains_an_in_flight_legacy_writer_before_freezing_the_ledger() {
        let (admin, pool, schema) = isolated_schema("m0091_concurrency").await;
        create_pre_m0091_schema(&pool).await;
        sqlx::raw_sql(
            r#"
            INSERT INTO "Games" (id) VALUES (1);
            INSERT INTO "GameChallenges" (id, game_id) VALUES (20, 1);
            INSERT INTO "Participations" (id, game_id, suspicion_score)
            VALUES (10, 1, 999);
            INSERT INTO "SuspicionRules" (rule_code, weight, description)
            VALUES ('StolenFlag', 100, 'frozen legacy weight');
            "#,
        )
        .execute(&pool)
        .await
        .expect("seed migration concurrency fixture");

        let mut legacy_writer = pool.begin().await.expect("begin legacy writer");
        sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR SHARE"#)
            .execute(&mut *legacy_writer)
            .await
            .expect("lock game like legacy detector");
        sqlx::query(r#"SELECT id FROM "GameChallenges" WHERE id = 20 FOR SHARE"#)
            .execute(&mut *legacy_writer)
            .await
            .expect("lock challenge like legacy detector");
        sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 10 FOR SHARE"#)
            .execute(&mut *legacy_writer)
            .await
            .expect("lock participation like legacy detector");
        sqlx::query(
            r#"INSERT INTO "SuspicionEvents"
               (game_id, participation_id, challenge_id, kind, evidence_key, score_delta)
               VALUES
                 (1, 10, NULL,  9, 'global',       NULL),
                 (1, 10, 20,   17, 'challenge:20', NULL),
                 (1, 10, 20,   20, 'challenge:20', NULL),
                 (1, 10, 20,   24, 'challenge:20', NULL),
                 (1, 10, 20,   27, 'challenge:20', NULL)"#,
        )
        .execute(&mut *legacy_writer)
        .await
        .expect("stage nullable events under the exact legacy canonical keys");

        let migration_pool = pool.clone();
        let migration =
            tokio::spawn(async move { sqlx::raw_sql(UP_SQL).execute(&migration_pool).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !migration.is_finished(),
            "migration must wait for the outer legacy-writer fence"
        );
        legacy_writer.commit().await.expect("commit legacy event");
        tokio::time::timeout(std::time::Duration::from_secs(10), migration)
            .await
            .expect("migration completes after legacy writer")
            .expect("join migration task")
            .expect("m0091 accepts and freezes the committed legacy event");

        let quarantined: Vec<(i32, i16, String, i32)> = sqlx::query_as(
            r#"SELECT id, kind, evidence_key, score_delta
                 FROM "SuspicionEvents"
                ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .expect("read quarantined migration result");
        assert_eq!(
            quarantined,
            vec![
                (1, 9, "legacy-untrusted:1".to_string(), 0),
                (2, 17, "legacy-untrusted:2".to_string(), 0),
                (3, 20, "legacy-untrusted:3".to_string(), 0),
                (4, 24, "legacy-untrusted:4".to_string(), 0),
                (5, 27, "legacy-untrusted:5".to_string(), 0),
            ],
            "every pre-cutover canonical collision is retained but made non-actionable",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*)
                     FROM "SuspicionEvents"
                    WHERE evidence_key NOT LIKE 'legacy-untrusted:%'"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "quarantined evidence must be excluded from the trusted ledger",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i32>(
                r#"SELECT suspicion_score FROM "Participations" WHERE id = 10"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "quarantined legacy evidence must contribute no score",
        );

        sqlx::query(
            r#"INSERT INTO "SuspicionEvents"
               (game_id, participation_id, challenge_id, kind, evidence_key, score_delta)
               VALUES
                 (1, 10, NULL,  9, 'global',        30),
                 (1, 10, 20,   17, 'challenge:20',  50),
                 (1, 10, 20,   20, 'challenge:20',  60),
                 (1, 10, 20,   24, 'challenge:20',  40),
                 (1, 10, 20,   27, 'challenge:20',  20)"#,
        )
        .execute(&pool)
        .await
        .expect("canonical detectors can reuse every key freed by quarantine");

        sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect("m0091 rerun remains idempotent");
        assert_eq!(
            sqlx::query_scalar::<_, i32>(
                r#"SELECT suspicion_score FROM "Participations" WHERE id = 10"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            65,
            "later canonical events remain trusted and use the capped score projection",
        );
        let canonical: Vec<(i16, Option<i32>, String, i32)> = sqlx::query_as(
            r#"SELECT kind, challenge_id, evidence_key, score_delta
                 FROM "SuspicionEvents"
                WHERE evidence_key NOT LIKE 'legacy-untrusted:%'
                ORDER BY kind"#,
        )
        .fetch_all(&pool)
        .await
        .expect("read canonical rows after idempotent rerun");
        assert_eq!(
            canonical,
            vec![
                (9, None, "global".to_string(), 30),
                (17, Some(20), "challenge:20".to_string(), 50),
                (20, Some(20), "challenge:20".to_string(), 60),
                (24, Some(20), "challenge:20".to_string(), 40),
                (27, Some(20), "challenge:20".to_string(), 20),
            ],
            "the durable cutover marker must not requarantine trusted rows on rerun",
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                r#"SELECT COUNT(*), COALESCE(SUM(score_delta), 0)
                     FROM "SuspicionEvents"
                    WHERE evidence_key LIKE 'legacy-untrusted:%'"#,
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            (5, 0),
            "rerunning the migration must preserve the quarantined forensic rows",
        );
        let mutation = sqlx::query(
            r#"UPDATE "SuspicionEvents" SET score_delta = 101
                WHERE evidence_key = 'legacy-untrusted:1'"#,
        )
        .execute(&pool)
        .await
        .expect_err("frozen event mutation must be rejected");
        assert!(mutation.to_string().contains("immutable evidence ledger"));

        let invalid_kind = sqlx::query(
            r#"INSERT INTO "SuspicionEvents"
               (game_id, participation_id, challenge_id, kind, evidence_key, score_delta)
               VALUES (1, 10, 20, 38, 'invalid-kind', 100)"#,
        )
        .execute(&pool)
        .await
        .expect_err("kind outside the stable 38-rule mapping must be rejected");
        assert!(matches!(
            &invalid_kind,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23514")
        ));
        let invalid_delta = sqlx::query(
            r#"INSERT INTO "SuspicionEvents"
               (game_id, participation_id, challenge_id, kind, evidence_key, score_delta)
               VALUES (1, 10, 20, 0, 'invalid-delta', 10001)"#,
        )
        .execute(&pool)
        .await
        .expect_err("unbounded frozen score delta must be rejected");
        assert!(matches!(
            &invalid_delta,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23514")
        ));
        let invalid_weight = sqlx::query(
            r#"UPDATE "SuspicionRules" SET weight = 10001 WHERE rule_code = 'StolenFlag'"#,
        )
        .execute(&pool)
        .await
        .expect_err("unbounded rule weight must be rejected");
        assert!(matches!(
            &invalid_weight,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23514")
        ));

        drop_isolated_schema(admin, pool, schema).await;
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_fails_closed_without_discarding_mismatched_provenance() {
        let (admin, pool, schema) = isolated_schema("m0091_provenance").await;
        create_pre_m0091_schema(&pool).await;
        sqlx::raw_sql(
            r#"
            INSERT INTO "Games" (id) VALUES (1), (2);
            INSERT INTO "GameChallenges" (id, game_id) VALUES (20, 1), (21, 2);
            INSERT INTO "Participations" (id, game_id, suspicion_score)
            VALUES (10, 2, 999);
            INSERT INTO "SuspicionEvents"
              (game_id, participation_id, challenge_id, kind, evidence_key, score_delta)
            VALUES (1, 10, NULL, 0, 'submission:1', 100);
            "#,
        )
        .execute(&pool)
        .await
        .expect("seed mismatched provenance fixture");

        let participation_error = sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect_err("participation mismatch must stop migration");
        assert!(participation_error
            .to_string()
            .contains("mismatched participation provenance"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "SuspicionEvents""#)
                .fetch_one(&pool)
                .await
                .expect("mismatched evidence remains preserved"),
            1
        );

        sqlx::query(
            r#"UPDATE "SuspicionEvents"
                  SET game_id = 2, challenge_id = 20
                WHERE evidence_key = 'submission:1'"#,
        )
        .execute(&pool)
        .await
        .expect("stage challenge mismatch after preserving row");
        let challenge_error = sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect_err("challenge mismatch must stop migration");
        assert!(challenge_error
            .to_string()
            .contains("mismatched challenge provenance"));

        sqlx::query(
            r#"UPDATE "SuspicionEvents" SET challenge_id = 21
                WHERE evidence_key = 'submission:1'"#,
        )
        .execute(&pool)
        .await
        .expect("repair provenance fixture");
        sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect("migration accepts repaired provenance");
        let foreign_key_error = sqlx::query(
            r#"INSERT INTO "SuspicionEvents"
               (game_id, participation_id, challenge_id, kind, evidence_key, score_delta)
               VALUES (1, 10, NULL, 0, 'submission:2', 100)"#,
        )
        .execute(&pool)
        .await
        .expect_err("provenance FK must reject a future mismatch");
        assert_eq!(
            foreign_key_error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("23503")
        );

        drop_isolated_schema(admin, pool, schema).await;
    }
}
