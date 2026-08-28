//! Durable identity and bounded recovery for bulk static-flag authoring.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "FlagImportOperations" (
    challenge_id      INTEGER NOT NULL,
    operation_id      UUID NOT NULL,
    actor_user_id     UUID NOT NULL,
    request_digest    BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
    state             SMALLINT NOT NULL DEFAULT 0 CHECK (state IN (0, 1)),
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
                         AND inserted_count IS NOT NULL AND duplicate_count IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS ix_flag_import_operations_retention
    ON "FlagImportOperations" (completed_at_utc, challenge_id, operation_id)
    WHERE state = 1;
WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (PARTITION BY challenge_id, flag ORDER BY id) AS ordinal
      FROM "FlagContexts"
     WHERE challenge_id IS NOT NULL
)
DELETE FROM "FlagContexts" context
 USING ranked
 WHERE context.id = ranked.id AND ranked.ordinal > 1;
CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag
    ON "FlagContexts" (challenge_id, flag)
    WHERE challenge_id IS NOT NULL;
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

INSERT INTO "FlagPolicyViolations"
    (challenge_id, flag_context_id, violation_type, observed_bytes)
SELECT flag.challenge_id, flag.id, 'flag_context', OCTET_LENGTH(flag.flag)
  FROM "FlagContexts" flag
 WHERE flag.challenge_id IS NOT NULL
   AND NOT (
       OCTET_LENGTH(flag.flag) BETWEEN 1 AND 127
       AND flag.flag !~ '(^[[:space:]])|([[:space:]]$)'
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
       AND flag.flag ~ '^flag[{][A-Za-z0-9_-]{32}[}]"#;

const DOWN_SQL: &str = r#"ALTER TABLE "FlagContexts"
    DROP CONSTRAINT IF EXISTS ck_flagcontexts_canonical_normal_flag;
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_gamechallenges_dynamic_flag_template;
ALTER TABLE "AdFlags"
    DROP CONSTRAINT IF EXISTS ck_adflags_canonical_flag;
ALTER TABLE "ChallengeVariants"
    DROP CONSTRAINT IF EXISTS ck_challengevariants_canonical_flag;
DROP TABLE IF EXISTS "FlagPolicyViolations";
DROP TABLE IF EXISTS "FlagImportOperations";
DROP INDEX IF EXISTS ux_flag_contexts_challenge_flag;
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

    #[test]
    fn imports_have_one_identity_bounded_results_and_indexed_duplicate_checks() {
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, operation_id)"));
        assert!(UP_SQL.contains("inserted_count <= 100"));
        assert!(
            UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag")
        );
    }
}

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
        AND variant.manifest->>'flag' !~ '(^[[:space:]])|([[:space:]]$)'
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
   AND (
       challenge.flag_template ~ '(^[[:space:]])|([[:space:]]$)'
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

DO $
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
              AND flag !~ '(^[[:space:]])|([[:space:]]$)'
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
              AND flag ~ '^flag[{][A-Za-z0-9_-]{32}[}]"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "FlagImportOperations";
DROP INDEX IF EXISTS ux_flag_contexts_challenge_flag;
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

    #[test]
    fn imports_have_one_identity_bounded_results_and_indexed_duplicate_checks() {
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, operation_id)"));
        assert!(UP_SQL.contains("inserted_count <= 100"));
        assert!(
            UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag")
        );
    }
}

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
              AND manifest->>'flag' !~ '(^[[:space:]])|([[:space:]]$)'
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
              OR (
                  flag_template !~ '(^[[:space:]])|([[:space:]]$)'
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
END $;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "FlagImportOperations";
DROP INDEX IF EXISTS ux_flag_contexts_challenge_flag;
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

    #[test]
    fn imports_have_one_identity_bounded_results_and_indexed_duplicate_checks() {
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, operation_id)"));
        assert!(UP_SQL.contains("inserted_count <= 100"));
        assert!(
            UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS ux_flag_contexts_challenge_flag")
        );
    }
}

