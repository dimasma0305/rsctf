//! Immutable, per-service receipts for each A&D flag-publication attempt.

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
                CREATE TABLE IF NOT EXISTS "AdFlagDeliveryResults" (
                  round_id INTEGER NOT NULL REFERENCES "AdRounds"(id) ON DELETE CASCADE,
                  team_service_id INTEGER NOT NULL
                    REFERENCES "AdTeamServices"(id) ON DELETE CASCADE,
                  delivery_kind TEXT NOT NULL,
                  container_id TEXT NULL,
                  delivered BOOLEAN NOT NULL,
                  attempts SMALLINT NOT NULL,
                  failure_reason TEXT NULL,
                  completed_at TIMESTAMPTZ NOT NULL,
                  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  PRIMARY KEY (round_id, team_service_id),
                  CONSTRAINT fk_ad_flag_delivery_flag
                    FOREIGN KEY (round_id, team_service_id)
                    REFERENCES "AdFlags"(round_id, team_service_id)
                    ON DELETE CASCADE,
                  CONSTRAINT ck_ad_flag_delivery_kind
                    CHECK (delivery_kind IN ('Managed', 'External')),
                  CONSTRAINT ck_ad_flag_delivery_container CHECK (
                    (delivery_kind = 'Managed'
                      AND (container_id IS NULL
                           OR NULLIF(BTRIM(container_id), '') IS NOT NULL))
                    OR (delivery_kind = 'External' AND container_id IS NULL)
                  ),
                  CONSTRAINT ck_ad_flag_delivery_attempts
                    CHECK (attempts BETWEEN 0 AND 5),
                  CONSTRAINT ck_ad_flag_delivery_outcome CHECK (
                    (delivered AND failure_reason IS NULL AND attempts >= 1)
                    OR (NOT delivered
                        AND NULLIF(BTRIM(failure_reason), '') IS NOT NULL)
                  )
                );

                CREATE INDEX IF NOT EXISTS ix_ad_flag_delivery_failed_round
                  ON "AdFlagDeliveryResults"(round_id, team_service_id)
                  WHERE delivered = FALSE;

                CREATE OR REPLACE FUNCTION rsctf_reject_ad_flag_delivery_update()
                RETURNS TRIGGER LANGUAGE plpgsql AS $$
                BEGIN
                  RAISE EXCEPTION 'A&D flag-delivery evidence is immutable'
                    USING ERRCODE = '55000';
                END
                $$;
                DROP TRIGGER IF EXISTS trg_ad_flag_delivery_immutable
                  ON "AdFlagDeliveryResults";
                CREATE TRIGGER trg_ad_flag_delivery_immutable
                  BEFORE UPDATE ON "AdFlagDeliveryResults"
                  FOR EACH ROW EXECUTE FUNCTION rsctf_reject_ad_flag_delivery_update();
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
                DROP INDEX IF EXISTS ix_ad_flag_delivery_failed_round;
                DROP TABLE IF EXISTS "AdFlagDeliveryResults";
                DROP FUNCTION IF EXISTS rsctf_reject_ad_flag_delivery_update();
                "#,
            )
            .await?;
        Ok(())
    }
}
