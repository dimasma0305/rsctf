//! Enforce the child-row identities used by the transactional A&D round writer.
//! Historical duplicates are consolidated before adding the indexes so retries can
//! use `ON CONFLICT DO NOTHING` without losing already-recorded attacks.

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
                LOCK TABLE "AdFlags", "AdAttacks", "KothTokens"
                  IN SHARE ROW EXCLUSIVE MODE;

                WITH flag_mapping AS (
                  SELECT id AS flag_id,
                         first_value(id) OVER (
                           PARTITION BY round_id, team_service_id ORDER BY id
                         ) AS retained_id
                    FROM "AdFlags"
                ), duplicate_attacks AS (
                  SELECT attack.id,
                         row_number() OVER (
                           PARTITION BY attack.attacker_participation_id,
                                        mapping.retained_id
                           ORDER BY attack.id
                         ) AS duplicate_number
                    FROM "AdAttacks" attack
                    JOIN flag_mapping mapping ON mapping.flag_id = attack.flag_id
                )
                DELETE FROM "AdAttacks" attack
                 USING duplicate_attacks duplicate
                 WHERE duplicate.duplicate_number > 1
                   AND attack.id = duplicate.id;

                WITH duplicate_flags AS (
                  SELECT id AS duplicate_id,
                         first_value(id) OVER (
                           PARTITION BY round_id, team_service_id ORDER BY id
                         ) AS retained_id,
                         row_number() OVER (
                           PARTITION BY round_id, team_service_id ORDER BY id
                         ) AS duplicate_number
                    FROM "AdFlags"
                )
                UPDATE "AdAttacks" attack
                   SET flag_id = duplicate.retained_id
                  FROM duplicate_flags duplicate
                 WHERE duplicate.duplicate_number > 1
                   AND attack.flag_id = duplicate.duplicate_id;

                DELETE FROM "AdFlags" flag
                 WHERE flag.id IN (
                       SELECT id
                         FROM (
                           SELECT id,
                                  row_number() OVER (
                                    PARTITION BY round_id, team_service_id ORDER BY id
                                  ) AS duplicate_number
                             FROM "AdFlags"
                         ) ranked
                        WHERE ranked.duplicate_number > 1
                 );

                DELETE FROM "KothTokens" token
                 WHERE token.id IN (
                       SELECT id
                         FROM (
                           SELECT id,
                                  row_number() OVER (
                                    PARTITION BY participation_id, round_number, ad_round_id
                                    ORDER BY id
                                  ) AS duplicate_number
                             FROM "KothTokens"
                            WHERE round_number IS NOT NULL
                              AND ad_round_id IS NOT NULL
                         ) ranked
                        WHERE ranked.duplicate_number > 1
                 );

                CREATE UNIQUE INDEX IF NOT EXISTS ux_adflags_round_service
                  ON "AdFlags"(round_id, team_service_id);
                CREATE UNIQUE INDEX IF NOT EXISTS ux_kothtokens_part_round_mint
                  ON "KothTokens"(participation_id, round_number, ad_round_id)
                  WHERE round_number IS NOT NULL AND ad_round_id IS NOT NULL;
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
                DROP INDEX IF EXISTS ux_kothtokens_part_round_mint;
                DROP INDEX IF EXISTS ux_adflags_round_service;
                "#,
            )
            .await?;
        Ok(())
    }
}
