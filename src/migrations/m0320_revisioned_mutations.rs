use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1;
ALTER TABLE "GameChallenges" ALTER COLUMN revision SET DEFAULT 1;

DO $$ BEGIN
    ALTER TABLE "GameChallenges"
        ADD CONSTRAINT ck_game_challenges_revision
        CHECK (revision BETWEEN 1 AND 9007199254740991);
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

CREATE TABLE IF NOT EXISTS "MutationOperations" (
    actor_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
    resource_kind TEXT NOT NULL,
    scope_key TEXT NOT NULL,
    operation_id UUID NOT NULL,
    request_fingerprint BYTEA NOT NULL
        CHECK (octet_length(request_fingerprint) = 32),
    result_id TEXT,
    result_revision BIGINT,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ,
    expires_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp() + interval '7 days',
    PRIMARY KEY (actor_id, resource_kind, scope_key, operation_id),
    CHECK (operation_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK ((result_id IS NULL) = (completed_at_utc IS NULL)),
    CHECK (result_revision IS NULL OR result_revision BETWEEN 1 AND 9007199254740991)
);

CREATE INDEX IF NOT EXISTS ix_mutation_operations_retention
    ON "MutationOperations" (expires_at_utc, actor_id, resource_kind);

CREATE TABLE IF NOT EXISTS "ChallengeDefinitionTransitions" (
    challenge_id INTEGER PRIMARY KEY REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    actor_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
    operation_id UUID NOT NULL,
    request_fingerprint BYTEA NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    expected_revision BIGINT NOT NULL CHECK (expected_revision BETWEEN 1 AND 9007199254740991),
    restore_enabled BOOLEAN NOT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (operation_id <> '00000000-0000-0000-0000-000000000000'::uuid)
);

CREATE INDEX IF NOT EXISTS ix_challenge_definition_transitions_age
    ON "ChallengeDefinitionTransitions" (created_at_utc, challenge_id);

CREATE TABLE IF NOT EXISTS "ChallengeRevisionEffects" (
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    effects JSONB NOT NULL,
    available_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_owner UUID,
    lease_expires_at_utc TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    completed_at_utc TIMESTAMPTZ,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (challenge_id, revision),
    CHECK (revision BETWEEN 1 AND 9007199254740991),
    CHECK (attempts BETWEEN 0 AND 1000000),
    CHECK (jsonb_typeof(effects) = 'object')
);

CREATE INDEX IF NOT EXISTS ix_challenge_revision_effects_due
    ON "ChallengeRevisionEffects" (available_at_utc, challenge_id, revision)
    WHERE completed_at_utc IS NULL;
CREATE INDEX IF NOT EXISTS ix_challenge_revision_effects_retention
    ON "ChallengeRevisionEffects" (completed_at_utc, challenge_id, revision)
    WHERE completed_at_utc IS NOT NULL;

CREATE TABLE IF NOT EXISTS "ChallengeRevisionNotices" (
    challenge_id INTEGER NOT NULL,
    revision BIGINT NOT NULL,
    notice_type SMALLINT NOT NULL CHECK (notice_type IN (4, 5)),
    notice_id INTEGER NOT NULL UNIQUE REFERENCES "GameNotices"(id) ON DELETE CASCADE,
    PRIMARY KEY (challenge_id, revision, notice_type),
    FOREIGN KEY (challenge_id, revision)
        REFERENCES "ChallengeRevisionEffects"(challenge_id, revision) ON DELETE CASCADE
);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "ChallengeRevisionNotices";
DROP TABLE IF EXISTS "ChallengeRevisionEffects";
DROP TABLE IF EXISTS "ChallengeDefinitionTransitions";
DROP TABLE IF EXISTS "MutationOperations";
ALTER TABLE "GameChallenges" DROP CONSTRAINT IF EXISTS ck_game_challenges_revision;
ALTER TABLE "GameChallenges" DROP COLUMN IF EXISTS revision;
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
    fn mutation_replays_and_revision_effects_are_durable_and_bounded() {
        assert!(UP_SQL.contains("PRIMARY KEY (actor_id, resource_kind, scope_key, operation_id)"));
        assert!(UP_SQL.contains("request_fingerprint BYTEA"));
        assert!(UP_SQL.contains("expires_at_utc"));
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, revision)"));
        assert!(UP_SQL.contains("ChallengeRevisionNotices"));
        assert!(UP_SQL.contains("ChallengeDefinitionTransitions"));
    }
}
