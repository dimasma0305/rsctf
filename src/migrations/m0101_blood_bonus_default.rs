//! Make challenge blood rewards opt-in for newly inserted challenges.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ALTER COLUMN disable_blood_bonus SET DEFAULT TRUE;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ALTER COLUMN disable_blood_bonus SET DEFAULT FALSE;
"#;

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
    fn new_challenges_default_to_no_blood_bonus() {
        assert!(UP_SQL.contains("disable_blood_bonus SET DEFAULT TRUE"));
        assert!(DOWN_SQL.contains("disable_blood_bonus SET DEFAULT FALSE"));
    }
}
