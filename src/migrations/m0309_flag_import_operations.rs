//! Durable identity and bounded recovery for bulk static-flag authoring.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
-- Keep this explicit alphabet byte-for-byte aligned with Rust flag_policy and
-- web FlagImport. It is Unicode White_Space plus browser-trimmed U+FEFF.
CREATE OR REPLACE FUNCTION rsctf_flag_is_whitespace(input_character TEXT)
RETURNS BOOLEAN
LANGUAGE SQL
IMMUTABLE
PARALLEL SAFE
STRICT
SET search_path FROM CURRENT
AS $function$
    SELECT input_character IN (
        CHR(9), CHR(10), CHR(11), CHR(12), CHR(13), CHR(32),
        CHR(133), CHR(160), CHR(5760),
        CHR(8192), CHR(8193), CHR(8194), CHR(8195), CHR(8196),
        CHR(8197), CHR(8198), CHR(8199), CHR(8200), CHR(8201), CHR(8202),
        CHR(8232), CHR(8233), CHR(8239), CHR(8287), CHR(12288), CHR(65279)
    )
$function$;

CREATE OR REPLACE FUNCTION rsctf_flag_has_boundary_whitespace(input_value TEXT)
RETURNS BOOLEAN
LANGUAGE SQL
IMMUTABLE
PARALLEL SAFE
STRICT
SET search_path FROM CURRENT
AS $function$
    SELECT input_value <> '' AND (
        rsctf_flag_is_whitespace(LEFT(input_value, 1))
        OR rsctf_flag_is_whitespace(RIGHT(input_value, 1))
    )
$function$;

CREATE OR REPLACE FUNCTION rsctf_flag_is_blank(input_value TEXT)
RETURNS BOOLEAN
LANGUAGE SQL
IMMUTABLE
PARALLEL SAFE
STRICT
SET search_path FROM CURRENT
AS $function$
    SELECT NOT EXISTS (
        SELECT 1
          FROM UNNEST(string_to_array(input_value, NULL)) AS codepoints(codepoint)
         WHERE NOT rsctf_flag_is_whitespace(codepoint)
    )
$function$;

CREATE TABLE IF NOT EXISTS "FlagImportOperations" (
    challenge_id      INTEGER NOT NULL,
    operation_id      UUID NOT NULL,
    actor_user_id     UUID NOT NULL,
    request_digest    BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
    state             SMALLINT NOT NULL DEFAULT 0 CHECK (state IN (0, 1, 2)),
    lease_token       UUID NOT NULL,
    inserted_count    INTEGER NULL CHECK (inserted_count >= 0 AND inserted_count <= 100),
    duplicate_count   INTEGER NULL CHECK (duplicate_count >= 0 AND duplicate_count <= 100),
    lease_expires_at_utc TIMESTAMPTZ NOT NULL
        DEFAULT (clock_timestamp() + INTERVAL '5 minutes'),
    completed_at_utc  TIMESTAMPTZ NULL,
    created_at_utc    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (challenge_id, operation_id),
    CONSTRAINT fk_flag_import_operation_challenge
        FOREIGN KEY (challenge_id) REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    CONSTRAINT fk_flag_import_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
    CONSTRAINT ck_flag_import_operation_result
        CHECK ((state = 0 AND completed_at_utc IS NULL
                         AND inserted_count IS NULL AND duplicate_count IS NULL)
               OR (state = 1 AND completed_at_utc IS NOT NULL
                         AND inserted_count IS NOT NULL AND duplicate_count IS NOT NULL)
               OR (state = 2 AND completed_at_utc IS NOT NULL
                         AND inserted_count IS NULL AND duplicate_count IS NULL))
);

CREATE TABLE IF NOT EXISTS "FlagImportSlots" (
    slot_id          SMALLINT PRIMARY KEY CHECK (slot_id BETWEEN 0 AND 3),
    lease_token      UUID NULL,
    expires_at_utc   TIMESTAMPTZ NULL,
    CHECK ((lease_token IS NULL) = (expires_at_utc IS NULL))
);
INSERT INTO "FlagImportSlots" (slot_id)
VALUES (0), (1), (2), (3)
ON CONFLICT DO NOTHING;
CREATE INDEX IF NOT EXISTS ix_flag_import_slot_expiry
    ON "FlagImportSlots" (expires_at_utc, slot_id);

CREATE INDEX IF NOT EXISTS ix_flag_import_operations_retention
    ON "FlagImportOperations" (completed_at_utc, challenge_id, operation_id)
    WHERE state IN (1, 2);
CREATE INDEX IF NOT EXISTS ix_flag_import_operations_abandoned
    ON "FlagImportOperations" (created_at_utc, lease_expires_at_utc,
                               challenge_id, operation_id)
    WHERE state = 0;
-- A recovery rerun can encounter the NOT VALID guards installed by an earlier
-- completed pass. PostgreSQL still enforces those guards for every updated
-- row, including quarantining an unchanged legacy value, so remove them before
-- touching legacy rows and recreate them after the audit is complete.
ALTER TABLE "FlagContexts"
    DROP CONSTRAINT IF EXISTS ck_flagcontexts_canonical_normal_flag;
ALTER TABLE "AdFlags"
    DROP CONSTRAINT IF EXISTS ck_adflags_canonical_flag;
ALTER TABLE "ChallengeVariants"
    DROP CONSTRAINT IF EXISTS ck_challengevariants_canonical_flag;
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_gamechallenges_dynamic_flag_template;

ALTER TABLE "FlagContexts"
    ADD COLUMN IF NOT EXISTS canonical_identity_enforced BOOLEAN NOT NULL DEFAULT TRUE;
UPDATE "FlagContexts"
   SET canonical_identity_enforced = FALSE
 WHERE challenge_id IS NOT NULL
   AND NOT (
       OCTET_LENGTH(flag) BETWEEN 1 AND 127
       AND NOT rsctf_flag_has_boundary_whitespace(flag)
   );
WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (PARTITION BY challenge_id, flag ORDER BY id) AS ordinal
      FROM "FlagContexts"
     WHERE challenge_id IS NOT NULL AND canonical_identity_enforced
)
UPDATE "FlagContexts" context
   SET canonical_identity_enforced = FALSE
  FROM ranked
 WHERE context.id = ranked.id AND ranked.ordinal > 1;
CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag
    ON "FlagContexts" (challenge_id, flag)
    WHERE challenge_id IS NOT NULL AND canonical_identity_enforced;
CREATE TABLE IF NOT EXISTS "FlagPolicyViolations" (
    id BIGSERIAL PRIMARY KEY,
    challenge_id INTEGER NOT NULL
        REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    flag_context_id INTEGER NULL,
    violation_type TEXT NOT NULL,
    observed_bytes BIGINT NOT NULL,
    detected_at_utc TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_policy_violations_identity
    ON "FlagPolicyViolations"
       (challenge_id, violation_type, COALESCE(flag_context_id, 0));

-- Empty author input has always selected the bounded default generator, and
-- the editor already persists that state as NULL. Canonicalize equivalent
-- legacy whitespace-only rows before auditing so a harmless representation
-- mismatch cannot disable an otherwise valid challenge.
UPDATE "GameChallenges"
   SET flag_template = NULL
 WHERE "Type" = 3
   AND flag_template IS NOT NULL
   AND rsctf_flag_is_blank(flag_template);

INSERT INTO "FlagPolicyViolations"
    (challenge_id, flag_context_id, violation_type, observed_bytes)
SELECT flag.challenge_id, flag.id, 'flag_context', OCTET_LENGTH(flag.flag)
  FROM "FlagContexts" flag
 WHERE flag.challenge_id IS NOT NULL
   AND NOT (
       OCTET_LENGTH(flag.flag) BETWEEN 1 AND 127
       AND NOT rsctf_flag_has_boundary_whitespace(flag.flag)
   )
ON CONFLICT DO NOTHING;

INSERT INTO "FlagPolicyViolations"
    (challenge_id, flag_context_id, violation_type, observed_bytes)
SELECT service.challenge_id, NULL, 'ad_flag:' || flag.id::TEXT,
       OCTET_LENGTH(flag.flag)
  FROM "AdFlags" flag
  JOIN "AdTeamServices" service ON service.id = flag.team_service_id
 WHERE NOT (
       OCTET_LENGTH(flag.flag) = 38
       AND flag.flag ~ '^flag[{][A-Za-z0-9_-]{32}[}]$'
   )
ON CONFLICT DO NOTHING;

INSERT INTO "FlagPolicyViolations"
    (challenge_id, flag_context_id, violation_type, observed_bytes)
SELECT variant.challenge_id, NULL, 'variant:' || variant.id::TEXT,
       COALESCE(OCTET_LENGTH(variant.manifest->>'flag'), 0)
  FROM "ChallengeVariants" variant
 WHERE jsonb_typeof(variant.manifest->'flag') IS DISTINCT FROM 'string'
    OR NOT (
        OCTET_LENGTH(variant.manifest->>'flag') BETWEEN 1 AND 127
        AND NOT rsctf_flag_has_boundary_whitespace(variant.manifest->>'flag')
    )
ON CONFLICT DO NOTHING;

INSERT INTO "FlagPolicyViolations"
    (challenge_id, flag_context_id, violation_type, observed_bytes)
SELECT challenge.id, NULL, 'dynamic_template',
       OCTET_LENGTH(challenge.flag_template)::BIGINT
         + 30::BIGINT * (
             (LENGTH(challenge.flag_template)
               - LENGTH(REPLACE(challenge.flag_template, '[GUID]', '')))
             / LENGTH('[GUID]')
           )
         + 30::BIGINT * (
             (LENGTH(challenge.flag_template)
               - LENGTH(REPLACE(challenge.flag_template, '[UUID]', '')))
             / LENGTH('[UUID]')
           )
         + 5::BIGINT * (
             (LENGTH(challenge.flag_template)
               - LENGTH(REPLACE(challenge.flag_template, '[TEAM_HASH]', '')))
             / LENGTH('[TEAM_HASH]')
           ) AS expanded_bytes
 FROM "GameChallenges" challenge
 WHERE challenge."Type" = 3
   AND challenge.flag_template IS NOT NULL
   AND challenge.flag_template <> ''
   AND (
       rsctf_flag_has_boundary_whitespace(challenge.flag_template)
       OR NOT (
           challenge.flag_template LIKE '%[GUID]%'
           OR challenge.flag_template LIKE '%[UUID]%'
           OR challenge.flag_template LIKE '%[TEAM_HASH]%'
       )
       OR OCTET_LENGTH(challenge.flag_template)::BIGINT
            + 30::BIGINT * (
                (LENGTH(challenge.flag_template)
                  - LENGTH(REPLACE(challenge.flag_template, '[GUID]', '')))
                / LENGTH('[GUID]')
              )
            + 30::BIGINT * (
                (LENGTH(challenge.flag_template)
                  - LENGTH(REPLACE(challenge.flag_template, '[UUID]', '')))
                / LENGTH('[UUID]')
              )
            + 5::BIGINT * (
                (LENGTH(challenge.flag_template)
                  - LENGTH(REPLACE(challenge.flag_template, '[TEAM_HASH]', '')))
                / LENGTH('[TEAM_HASH]')
              ) > 127
   )
ON CONFLICT DO NOTHING;

UPDATE "GameChallenges" challenge
   SET is_enabled = FALSE
 WHERE challenge.is_enabled = TRUE
   AND EXISTS (
       SELECT 1 FROM "FlagPolicyViolations" violation
        WHERE violation.challenge_id = challenge.id
   );

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_flagcontexts_canonical_normal_flag'
           AND conrelid = '"FlagContexts"'::regclass
    ) THEN
        ALTER TABLE "FlagContexts"
          ADD CONSTRAINT ck_flagcontexts_canonical_normal_flag
          CHECK (
              OCTET_LENGTH(flag) BETWEEN 1 AND 127
              AND NOT rsctf_flag_has_boundary_whitespace(flag)
          ) NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_adflags_canonical_flag'
           AND conrelid = '"AdFlags"'::regclass
    ) THEN
        ALTER TABLE "AdFlags"
          ADD CONSTRAINT ck_adflags_canonical_flag
          CHECK (
              OCTET_LENGTH(flag) = 38
              AND flag ~ '^flag[{][A-Za-z0-9_-]{32}[}]$'
          ) NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_challengevariants_canonical_flag'
           AND conrelid = '"ChallengeVariants"'::regclass
    ) THEN
        ALTER TABLE "ChallengeVariants"
          ADD CONSTRAINT ck_challengevariants_canonical_flag
          CHECK (
              COALESCE(jsonb_typeof(manifest->'flag') = 'string', FALSE)
              AND OCTET_LENGTH(manifest->>'flag') BETWEEN 1 AND 127
              AND NOT rsctf_flag_has_boundary_whitespace(manifest->>'flag')
          ) NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_gamechallenges_dynamic_flag_template'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
          ADD CONSTRAINT ck_gamechallenges_dynamic_flag_template
          CHECK (
              "Type" <> 3
              OR flag_template IS NULL
              OR flag_template = ''
              OR (
                  NOT rsctf_flag_has_boundary_whitespace(flag_template)
                  AND (
                      flag_template LIKE '%[GUID]%'
                      OR flag_template LIKE '%[UUID]%'
                      OR flag_template LIKE '%[TEAM_HASH]%'
                  )
                  AND OCTET_LENGTH(flag_template)::BIGINT
                        + 30::BIGINT * (
                            (LENGTH(flag_template)
                              - LENGTH(REPLACE(flag_template, '[GUID]', '')))
                            / LENGTH('[GUID]')
                          )
                        + 30::BIGINT * (
                            (LENGTH(flag_template)
                              - LENGTH(REPLACE(flag_template, '[UUID]', '')))
                            / LENGTH('[UUID]')
                          )
                        + 5::BIGINT * (
                            (LENGTH(flag_template)
                              - LENGTH(REPLACE(flag_template, '[TEAM_HASH]', '')))
                            / LENGTH('[TEAM_HASH]')
                          ) <= 127
              )
          ) NOT VALID;
    END IF;
END $$;

"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "FlagContexts"
    DROP CONSTRAINT IF EXISTS ck_flagcontexts_canonical_normal_flag;
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_gamechallenges_dynamic_flag_template;
ALTER TABLE "AdFlags"
    DROP CONSTRAINT IF EXISTS ck_adflags_canonical_flag;
ALTER TABLE "ChallengeVariants"
    DROP CONSTRAINT IF EXISTS ck_challengevariants_canonical_flag;
DROP TABLE IF EXISTS "FlagPolicyViolations";
DROP TABLE IF EXISTS "FlagImportOperations";
DROP TABLE IF EXISTS "FlagImportSlots";
DROP INDEX IF EXISTS ux_flag_contexts_challenge_flag;
ALTER TABLE "FlagContexts"
    DROP COLUMN IF EXISTS canonical_identity_enforced;
DROP FUNCTION IF EXISTS rsctf_flag_is_blank(TEXT);
DROP FUNCTION IF EXISTS rsctf_flag_has_boundary_whitespace(TEXT);
DROP FUNCTION IF EXISTS rsctf_flag_is_whitespace(TEXT);
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
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn imports_have_one_identity_bounded_results_and_indexed_duplicate_checks() {
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, operation_id)"));
        assert!(UP_SQL.contains("lease_token       UUID NOT NULL"));
        assert!(UP_SQL.contains("inserted_count <= 100"));
        assert!(UP_SQL.contains("ix_flag_import_operations_abandoned"));
        assert!(UP_SQL.contains("FlagImportSlots"));
        assert!(UP_SQL.contains("state IN (0, 1, 2)"));
        assert!(
            UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag")
        );
        assert!(UP_SQL.contains("canonical_identity_enforced = FALSE"));
        assert!(!UP_SQL.contains("DELETE FROM \"FlagContexts\" context"));
    }

    #[test]
    fn canonical_policy_reports_and_disables_without_truncating_legacy_answers() {
        assert!(UP_SQL.contains("FlagPolicyViolations"));
        assert!(UP_SQL.contains("OCTET_LENGTH(flag.flag) BETWEEN 1 AND 127"));
        assert!(UP_SQL.contains("ck_adflags_canonical_flag"));
        assert!(UP_SQL.contains("ck_challengevariants_canonical_flag"));
        assert!(UP_SQL.contains("SET is_enabled = FALSE"));
        assert!(UP_SQL.contains("NOT VALID"));
        assert!(!UP_SQL.contains("LEFT(flag.flag"));
        assert!(!UP_SQL.contains("SET flag ="));
    }

    #[test]
    fn database_template_formula_matches_runtime_replacement_deltas() {
        assert!(UP_SQL.contains("CREATE OR REPLACE FUNCTION rsctf_flag_is_whitespace"));
        assert!(UP_SQL.contains("CREATE OR REPLACE FUNCTION rsctf_flag_is_blank"));
        assert!(UP_SQL.contains("IMMUTABLE"));
        assert!(UP_SQL.contains("CHR(160)"));
        assert!(UP_SQL.contains("CHR(8195)"));
        assert!(UP_SQL.contains("CHR(65279)"));
        assert!(UP_SQL.contains("rsctf_flag_is_blank(flag_template)"));
        assert!(!UP_SQL.contains("flag_template ~"));
        assert!(UP_SQL.contains("OR flag_template = ''"));
        assert!(UP_SQL.contains("+ 30::BIGINT *"));
        assert!(UP_SQL.contains("REPLACE(flag_template, '[GUID]', '')"));
        assert!(UP_SQL.contains("REPLACE(flag_template, '[UUID]', '')"));
        assert!(UP_SQL.contains("+ 5::BIGINT *"));
        assert!(UP_SQL.contains("REPLACE(flag_template, '[TEAM_HASH]', '')"));
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn oversized_legacy_flag_is_excluded_before_unique_index_creation() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("flag_index_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY,
                "Type" SMALLINT NOT NULL DEFAULT 0,
                flag_template TEXT NULL,
                is_enabled BOOLEAN NOT NULL DEFAULT TRUE
            );
            CREATE TABLE "FlagContexts" (
                id SERIAL PRIMARY KEY,
                challenge_id INTEGER NULL REFERENCES "GameChallenges"(id),
                flag TEXT NOT NULL,
                is_occupied BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "AdTeamServices" (
                id INTEGER PRIMARY KEY,
                challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id)
            );
            CREATE TABLE "AdFlags" (
                id INTEGER PRIMARY KEY,
                team_service_id INTEGER NOT NULL REFERENCES "AdTeamServices"(id),
                flag TEXT NOT NULL
            );
            CREATE TABLE "ChallengeVariants" (
                id UUID PRIMARY KEY,
                challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id),
                manifest JSONB NOT NULL
            );
            INSERT INTO "GameChallenges" (id) VALUES (1);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut state = 0x9e37_79b9_u32;
        let oversized_flag = (0..8_192)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                char::from(b'!' + (state % 94) as u8)
            })
            .collect::<String>();
        assert!(oversized_flag.len() > 3 * 1_024);
        sqlx::query(r#"INSERT INTO "FlagContexts" (challenge_id, flag) VALUES (1, $1)"#)
            .bind(&oversized_flag)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let (canonical, enabled, violations) = sqlx::query_as::<_, (bool, bool, i64)>(
            r#"SELECT context.canonical_identity_enforced,
                      challenge.is_enabled,
                      (SELECT COUNT(*)::bigint FROM "FlagPolicyViolations")
                 FROM "FlagContexts" context
                 JOIN "GameChallenges" challenge ON challenge.id = context.challenge_id
                WHERE context.challenge_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!canonical);
        assert!(!enabled);
        assert_eq!(violations, 1);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
