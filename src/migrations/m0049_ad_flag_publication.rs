//! Durable completion marker for the A&D round flag-publication phase.

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
                ALTER TABLE "AdRounds"
                  ADD COLUMN IF NOT EXISTS flags_published_at TIMESTAMPTZ NULL,
                  ADD COLUMN IF NOT EXISTS flag_delivery_failures INTEGER NOT NULL DEFAULT 0;

                UPDATE "AdRounds"
                   SET flags_published_at = pipeline_completed_at
                 WHERE flags_published_at IS NULL
                   AND pipeline_completed_at IS NOT NULL;

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_adrounds_flag_delivery_failures'
                       AND conrelid = '"AdRounds"'::regclass
                  ) THEN
                    ALTER TABLE "AdRounds"
                      ADD CONSTRAINT ck_adrounds_flag_delivery_failures
                      CHECK (flag_delivery_failures >= 0);
                  END IF;
                END
                $$;
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
                ALTER TABLE "AdRounds"
                  DROP CONSTRAINT IF EXISTS ck_adrounds_flag_delivery_failures,
                  DROP COLUMN IF EXISTS flag_delivery_failures,
                  DROP COLUMN IF EXISTS flags_published_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
