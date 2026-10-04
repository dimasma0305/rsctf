//! One flag per team in per-participation challenge variants.
//!
//! Grading tells teams apart only by their variant's flag. If a generator
//! repeated a flag, a team submitting its own answer could be matched to
//! another team's variant and recorded as `CheatDetected`. The index makes a
//! repeated frozen flag impossible; generation reports it to the organizer.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS ux_challenge_variant_flag
    ON "ChallengeVariants"(game_id, challenge_id, (manifest->>'flag'))
    WHERE frozen_at_utc IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ux_challenge_variant_flag;
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
    fn frozen_variant_flags_are_unique_per_challenge() {
        assert!(UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS ux_challenge_variant_flag"));
        assert!(UP_SQL.contains("(game_id, challenge_id, (manifest->>'flag'))"));
        assert!(UP_SQL.contains("WHERE frozen_at_utc IS NOT NULL"));
    }
}
