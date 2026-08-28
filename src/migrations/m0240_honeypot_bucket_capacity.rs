use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "HoneypotBucketBudget" (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    row_count BIGINT NOT NULL DEFAULT 0 CHECK (row_count >= 0),
    reconciled_at_utc TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO "HoneypotBucketBudget" (singleton, row_count, reconciled_at_utc)
SELECT TRUE, COUNT(*)::BIGINT, CURRENT_TIMESTAMP
  FROM "HoneypotHitBuckets"
ON CONFLICT (singleton) DO UPDATE
      SET row_count = EXCLUDED.row_count,
          reconciled_at_utc = EXCLUDED.reconciled_at_utc;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only. The counter is harmless if
        // an older binary is restored and preserves the capacity state needed
        // when the current binary returns.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_counter_is_idempotent_and_seeded_from_preserved_rows() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"HoneypotBucketBudget\""));
        assert!(UP_SQL.contains("COUNT(*)::BIGINT"));
        assert!(UP_SQL.contains("ON CONFLICT (singleton) DO UPDATE"));
    }
}
