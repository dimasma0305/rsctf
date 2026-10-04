//! Ordered candidate access for bounded Docker image cleanup passes.

use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
-- m0282's claim-state-first index remains useful for exact backlog counts.
-- This complementary index owns the LIMIT query's stable rotation order so a
-- large installation can find the next small batch without sorting the whole
-- eligible ownership catalog while holding row locks.
CREATE INDEX IF NOT EXISTS ix_build_image_cleanup_ordered_candidates
    ON "BuildImageOwnerships" (
        installation_scope,
        cleanup_checked_at_utc ASC NULLS FIRST,
        (COALESCE(last_used_at_utc, updated_at_utc)),
        canonical_ref
    ) INCLUDE (cleanup_claim_until);
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
        // Forward-only: dropping the ordered access path can return scheduled
        // cleanup to catalog-wide top-N sorts during a rolling deployment.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn index_order_matches_the_bounded_candidate_claim() {
        assert!(UP_SQL.contains("ix_build_image_cleanup_ordered_candidates"));
        assert!(UP_SQL.contains("cleanup_checked_at_utc ASC NULLS FIRST"));
        assert!(UP_SQL.contains("COALESCE(last_used_at_utc, updated_at_utc)"));
        assert!(UP_SQL.contains("canonical_ref"));
    }
}
