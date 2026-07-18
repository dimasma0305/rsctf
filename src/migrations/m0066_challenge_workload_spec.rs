//! Add an optional, validated worker workload definition to Jeopardy container
//! challenges. The JSON stays nullable so existing single-container challenges
//! keep their current lifecycle and wire contract.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    ALTER TABLE "GameChallenges"
        ADD COLUMN IF NOT EXISTS workload_spec JSONB NULL;

    DO $$
    BEGIN
      IF EXISTS (
        SELECT 1
          FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'GameChallenges'
           AND column_name = 'workload_spec'
           AND data_type = 'json'
      ) THEN
        ALTER TABLE "GameChallenges"
          ALTER COLUMN workload_spec TYPE JSONB USING workload_spec::jsonb;
      END IF;
    END
    $$;

    DO $$
    BEGIN
      IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'ck_gamechallenges_workload_spec_object'
           AND conrelid = '"GameChallenges"'::regclass
      ) THEN
        ALTER TABLE "GameChallenges"
          ADD CONSTRAINT ck_gamechallenges_workload_spec_object
          CHECK (workload_spec IS NULL OR jsonb_typeof(workload_spec) = 'object');
      END IF;
    END
    $$;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE "GameChallenges"
                       DROP CONSTRAINT IF EXISTS ck_gamechallenges_workload_spec_object;
                   ALTER TABLE "GameChallenges"
                       DROP COLUMN IF EXISTS workload_spec;"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn adds_nullable_jsonb_column_idempotently() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS workload_spec JSONB NULL"));
        assert!(UP_SQL.contains("data_type = 'json'"));
        assert!(UP_SQL.contains("ALTER COLUMN workload_spec TYPE JSONB USING workload_spec::jsonb"));
        assert!(UP_SQL.contains("ck_gamechallenges_workload_spec_object"));
        assert!(UP_SQL.contains("jsonb_typeof(workload_spec) = 'object'"));
    }
}
