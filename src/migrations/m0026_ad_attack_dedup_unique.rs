//! Unique index making the A&D capture dedup race-safe (#14). `submit_one` was a
//! check-then-act: `SELECT … WHERE attacker=… AND flag=…` then INSERT, so N concurrent
//! identical-flag submits from one team all saw "no existing row" and inserted N
//! duplicate `AdAttacks` — inflating the attacker's capture count / attack share. With
//! this unique index the submit becomes `INSERT … ON CONFLICT DO NOTHING`, so only the
//! first capture lands. De-dups any existing duplicates first. Idempotent.
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
                DELETE FROM "AdAttacks" WHERE id IN (
                  SELECT id FROM (
                    SELECT id, row_number() OVER
                      (PARTITION BY attacker_participation_id, flag_id ORDER BY id) rn
                    FROM "AdAttacks"
                  ) t WHERE rn > 1
                );
                CREATE UNIQUE INDEX IF NOT EXISTS ux_adattacks_attacker_flag
                  ON "AdAttacks"(attacker_participation_id, flag_id);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP INDEX IF EXISTS ux_adattacks_attacker_flag;"#)
            .await?;
        Ok(())
    }
}
