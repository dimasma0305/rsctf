//! Remove orphaned KotH control capabilities and enforce participation cleanup.

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
                LOCK TABLE "KothTokens", "KothTargets", "Participations"
                  IN SHARE ROW EXCLUSIVE MODE;

                DELETE FROM "KothTokens" token
                 WHERE NOT EXISTS (
                       SELECT 1
                         FROM "Participations" participation
                        WHERE participation.id = token.participation_id
                          AND participation.status = 1
                 );

                UPDATE "KothTargets" target
                   SET holder_participation_id = NULL, held_since = NULL
                 WHERE target.holder_participation_id IS NOT NULL
                   AND NOT EXISTS (
                       SELECT 1
                         FROM "Participations" participation
                        WHERE participation.id = target.holder_participation_id
                          AND participation.game_id = target.game_id
                          AND participation.status = 1
                 );

                DO $$ BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtokens_participation'
                       AND conrelid = '"KothTokens"'::regclass
                  ) THEN
                    ALTER TABLE "KothTokens"
                      ADD CONSTRAINT fk_kothtokens_participation
                      FOREIGN KEY (participation_id) REFERENCES "Participations"(id)
                      ON DELETE CASCADE;
                  END IF;
                END $$;

                DO $$ BEGIN
                  IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                     WHERE conname = 'fk_kothtargets_holder_participation'
                       AND conrelid = '"KothTargets"'::regclass
                  ) THEN
                    ALTER TABLE "KothTargets"
                      ADD CONSTRAINT fk_kothtargets_holder_participation
                      FOREIGN KEY (holder_participation_id) REFERENCES "Participations"(id)
                      ON DELETE SET NULL;
                  END IF;
                END $$;
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
                ALTER TABLE "KothTargets"
                  DROP CONSTRAINT IF EXISTS fk_kothtargets_holder_participation;
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_participation;
                "#,
            )
            .await?;
        Ok(())
    }
}
