//! Preserve KotH token issuance while allowing bearer capabilities to be revoked.

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
                ALTER TABLE "KothTokens"
                  ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMPTZ NULL;

                DO $$
                BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'ck_kothtokens_revoked_after_issue'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT ck_kothtokens_revoked_after_issue
                      CHECK (revoked_at IS NULL OR revoked_at >= submitted_at);
                  END IF;
                END
                $$;

                CREATE INDEX IF NOT EXISTS ix_kothtokens_active_round_token
                  ON "KothTokens"(round_number, token, participation_id)
                  WHERE revoked_at IS NULL AND round_number IS NOT NULL;
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
                DROP INDEX IF EXISTS ix_kothtokens_active_round_token;
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS ck_kothtokens_revoked_after_issue,
                  DROP COLUMN IF EXISTS revoked_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
