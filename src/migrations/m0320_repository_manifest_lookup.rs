//! Bounded access path for repository manifest identity reconciliation.

use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_gamechallenges_repository_source_suffix
    ON "GameChallenges" (
        game_id,
        (reverse(replace(source_yaml_path, E'\\', '/'))) text_pattern_ops,
        id
    )
    WHERE source_yaml_path IS NOT NULL;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn repository_lookup_index_is_idempotent_and_matches_the_query_expression() {
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS"));
        assert!(UP_SQL.contains("ix_gamechallenges_repository_source_suffix"));
        assert!(UP_SQL.contains("reverse(replace(source_yaml_path, E'\\\\', '/'))"));
        assert!(UP_SQL.contains("text_pattern_ops"));
        assert!(UP_SQL.contains("WHERE source_yaml_path IS NOT NULL"));
    }
}
