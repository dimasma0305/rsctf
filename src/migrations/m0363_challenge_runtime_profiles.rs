use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            r#"ALTER TABLE "GameChallenges" ADD COLUMN IF NOT EXISTS runtime_profile VARCHAR(40) NULL;
            CREATE TABLE IF NOT EXISTS "RuntimeProfileBindings" (
                id VARCHAR(40) PRIMARY KEY,
                identity VARCHAR(16) NOT NULL
            );"#,
        ).await?;
        Ok(())
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
