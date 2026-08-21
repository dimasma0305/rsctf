//! Auto-build trusted `generator/Dockerfile` provenance sources while keeping
//! the author-facing manifest free of deployment-generated image identities.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS variant_generator_build_context_subdir TEXT NULL,
    ADD COLUMN IF NOT EXISTS variant_generator_build_status SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS variant_generator_last_build_log TEXT NULL;

ALTER TABLE "GameChallenges"
    ALTER COLUMN variant_generator_build_status SET DEFAULT 0,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_config,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_generator_build;

ALTER TABLE "GameChallenges"
    ADD CONSTRAINT ck_game_challenges_variant_generator_build CHECK (
        (
            variant_generator_build_context_subdir IS NULL
            AND variant_generator_build_status = 0
        )
        OR (
            variant_generator_build_context_subdir = 'generator'
            AND variant_generator_build_status IN (1, 2, 3, 5, 6)
        )
    ),
    ADD CONSTRAINT ck_game_challenges_variant_config CHECK (
        variant_mode = 0
        OR (
            variant_generator_build_context_subdir IS NULL
            AND variant_generator_image IS NOT NULL
            AND LENGTH(BTRIM(variant_generator_image)) BETWEEN 1 AND 512
            AND variant_generator_digest ~ '^sha256:[0-9a-f]{64}$'
        )
        OR (
            variant_generator_build_context_subdir = 'generator'
            AND (
                (
                    variant_generator_build_status = 1
                    AND variant_generator_image = variant_generator_digest
                    AND variant_generator_digest ~ '^sha256:[0-9a-f]{64}$'
                )
                OR (
                    variant_generator_build_status <> 1
                    AND variant_generator_image IS NULL
                    AND variant_generator_digest IS NULL
                )
            )
        )
    );
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_config,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_generator_build;

UPDATE "GameChallenges"
   SET variant_mode = 0,
       variant_generator_image = NULL,
       variant_generator_digest = NULL
 WHERE variant_generator_build_context_subdir IS NOT NULL
   AND (variant_generator_image IS NULL OR variant_generator_digest IS NULL);

ALTER TABLE "GameChallenges"
    DROP COLUMN IF EXISTS variant_generator_last_build_log,
    DROP COLUMN IF EXISTS variant_generator_build_status,
    DROP COLUMN IF EXISTS variant_generator_build_context_subdir;

ALTER TABLE "GameChallenges"
    ADD CONSTRAINT ck_game_challenges_variant_config CHECK (
        variant_mode = 0
        OR (
            variant_generator_image IS NOT NULL
            AND LENGTH(BTRIM(variant_generator_image)) BETWEEN 1 AND 512
            AND variant_generator_digest ~ '^sha256:[0-9a-f]{64}$'
        )
    );
"#;

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
mod tests {
    use super::*;

    #[test]
    fn auto_builds_are_queued_without_pretending_a_mutable_tag_is_immutable() {
        assert!(UP_SQL.contains("variant_generator_build_context_subdir = 'generator'"));
        assert!(UP_SQL.contains("variant_generator_build_status <> 1"));
        assert!(UP_SQL.contains("variant_generator_image IS NULL"));
        assert!(UP_SQL.contains("variant_generator_image = variant_generator_digest"));
    }
}
