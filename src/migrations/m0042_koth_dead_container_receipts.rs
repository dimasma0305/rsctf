//! Binds terminal KotH checker evidence to the exact dead backend it observed.

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
                ALTER TABLE "KothControlResults"
                  ADD COLUMN IF NOT EXISTS dead_container_id TEXT NULL;

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_kothcontrol_dead_container_receipt'
                       AND conrelid = '"KothControlResults"'::regclass
                  ) THEN
                    ALTER TABLE "KothControlResults"
                      ADD CONSTRAINT ck_kothcontrol_dead_container_receipt CHECK (
                        dead_container_id IS NULL OR (
                          NULLIF(BTRIM(dead_container_id), '') IS NOT NULL
                          AND status = 2
                          AND controlling_participation_id IS NULL
                          AND marker_observed = FALSE
                        )
                      );
                  END IF;
                END
                $$;

                CREATE INDEX IF NOT EXISTS ix_kothcontrol_dead_container_receipt
                  ON "KothControlResults"(game_id, challenge_id, checked_at DESC)
                  WHERE dead_container_id IS NOT NULL;
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
                DROP INDEX IF EXISTS ix_kothcontrol_dead_container_receipt;
                ALTER TABLE "KothControlResults"
                  DROP CONSTRAINT IF EXISTS ck_kothcontrol_dead_container_receipt,
                  DROP COLUMN IF EXISTS dead_container_id;
                "#,
            )
            .await?;
        Ok(())
    }
}
