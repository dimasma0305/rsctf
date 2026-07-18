//! Indexes on `round_id` for `AdCheckResults` and `AdAttacks` to optimize the scoreboard recompute queries.
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
                CREATE INDEX IF NOT EXISTS ix_adcheckresults_round
                  ON "AdCheckResults"(round_id);
                CREATE INDEX IF NOT EXISTS ix_adattacks_round
                  ON "AdAttacks"(round_id);
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
                DROP INDEX IF EXISTS ix_adcheckresults_round;
                DROP INDEX IF EXISTS ix_adattacks_round;
                "#,
            )
            .await?;
        Ok(())
    }
}
