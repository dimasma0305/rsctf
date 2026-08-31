use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS scan_lease_owner UUID;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS scan_lease_expires_at_utc TIMESTAMPTZ;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS scan_attempt BIGINT NOT NULL DEFAULT 0;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS scan_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS current_activity TEXT;
ALTER TABLE "RepoBindings" ALTER COLUMN scan_attempt SET DEFAULT 0;
ALTER TABLE "RepoBindings" ALTER COLUMN scan_failures SET DEFAULT 0;

UPDATE "RepoBindings"
   SET interval_seconds = LEAST(86400, GREATEST(60, interval_seconds)),
       next_scan_utc = COALESCE(next_scan_utc, clock_timestamp())
 WHERE interval_seconds NOT BETWEEN 60 AND 86400 OR next_scan_utc IS NULL;

DO $$ BEGIN
    ALTER TABLE "RepoBindings" ADD CONSTRAINT ck_repo_bindings_interval
        CHECK (interval_seconds BETWEEN 60 AND 86400);
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
DO $$ BEGIN
    ALTER TABLE "RepoBindings" ADD CONSTRAINT ck_repo_bindings_scan_failures
        CHECK (scan_failures BETWEEN 0 AND 1000000 AND scan_attempt >= 0);
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

CREATE INDEX IF NOT EXISTS ix_repo_bindings_due_claim
    ON "RepoBindings" (next_scan_utc, id)
    WHERE status = 0;

CREATE INDEX IF NOT EXISTS ix_repo_bindings_active_host_lease
    ON "RepoBindings" (
        (lower(split_part(split_part(repo_url, '://', 2), '/', 1))),
        scan_lease_expires_at_utc
    ) WHERE scan_lease_expires_at_utc IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_repo_binding_scans_history
    ON "RepoBindingScans" (binding_id, id DESC);

CREATE TABLE IF NOT EXISTS "RepoPushQueue" (
    binding_id INTEGER NOT NULL REFERENCES "RepoBindings"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    target_revision BIGINT NOT NULL CHECK (target_revision BETWEEN 1 AND 9007199254740991),
    available_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_owner UUID,
    lease_expires_at_utc TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (binding_id, challenge_id),
    CHECK (attempts BETWEEN 0 AND 1000000)
);

CREATE INDEX IF NOT EXISTS ix_repo_push_queue_due
    ON "RepoPushQueue" (available_at_utc, binding_id, challenge_id);

CREATE INDEX IF NOT EXISTS ix_repo_push_queue_binding_lease
    ON "RepoPushQueue" (binding_id, lease_expires_at_utc);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "RepoPushQueue";
DROP INDEX IF EXISTS ix_repo_binding_scans_history;
DROP INDEX IF EXISTS ix_repo_bindings_active_host_lease;
DROP INDEX IF EXISTS ix_repo_bindings_due_claim;
ALTER TABLE "RepoBindings" DROP CONSTRAINT IF EXISTS ck_repo_bindings_scan_failures;
ALTER TABLE "RepoBindings" DROP CONSTRAINT IF EXISTS ck_repo_bindings_interval;
ALTER TABLE "RepoBindings" DROP COLUMN IF EXISTS current_activity;
ALTER TABLE "RepoBindings" DROP COLUMN IF EXISTS scan_failures;
ALTER TABLE "RepoBindings" DROP COLUMN IF EXISTS scan_attempt;
ALTER TABLE "RepoBindings" DROP COLUMN IF EXISTS scan_lease_expires_at_utc;
ALTER TABLE "RepoBindings" DROP COLUMN IF EXISTS scan_lease_owner;
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
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn repo_claims_and_pushes_are_durable_bounded_and_coalescing() {
        assert!(UP_SQL.contains("interval_seconds BETWEEN 60 AND 86400"));
        assert!(UP_SQL.contains("scan_lease_expires_at_utc"));
        assert!(UP_SQL.contains("ix_repo_bindings_due_claim"));
        assert!(UP_SQL.contains("ix_repo_bindings_active_host_lease"));
        assert!(UP_SQL.contains("WHERE scan_lease_expires_at_utc IS NOT NULL"));
        assert!(UP_SQL.contains("ix_repo_binding_scans_history"));
        assert!(UP_SQL.contains("PRIMARY KEY (binding_id, challenge_id)"));
        assert!(UP_SQL.contains("target_revision"));
        assert!(UP_SQL.contains("ix_repo_push_queue_binding_lease"));
    }
}
