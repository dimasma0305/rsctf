//! Durable, revision-fenced KotH referee credential mutations.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "KothApiObserverRevisions" (
    challenge_id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL,
    revision BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_koth_api_observer_revisions_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_api_observer_revisions_revision
        CHECK (revision BETWEEN 0 AND 9007199254740991)
);

INSERT INTO "KothApiObserverRevisions"
    (challenge_id, game_id, revision, updated_at)
SELECT observer.challenge_id, observer.game_id, 1, observer.rotated_at
  FROM "KothApiObservers" observer
ON CONFLICT (challenge_id) DO NOTHING;

CREATE INDEX IF NOT EXISTS ix_koth_api_observer_revisions_game
    ON "KothApiObserverRevisions"(game_id, challenge_id);

CREATE TABLE IF NOT EXISTS "KothApiObserverOperations" (
    operation_id UUID PRIMARY KEY,
    challenge_id INTEGER NOT NULL,
    game_id INTEGER NOT NULL,
    actor_user_id UUID NULL,
    operation_kind VARCHAR(8) NOT NULL,
    expected_revision BIGINT NOT NULL,
    result_revision BIGINT NULL,
    result JSONB NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ NULL,
    expires_at TIMESTAMPTZ NOT NULL
        DEFAULT (clock_timestamp() + interval '24 hours'),
    disclosure_count INTEGER NOT NULL DEFAULT 0,
    last_disclosed_at TIMESTAMPTZ NULL,
    CONSTRAINT fk_koth_api_observer_operations_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_observer_operations_actor
        FOREIGN KEY (actor_user_id)
        REFERENCES "AspNetUsers"(id)
        ON DELETE SET NULL,
    CONSTRAINT ck_koth_api_observer_operations_kind
        CHECK (operation_kind IN ('Rotate', 'Revoke')),
    CONSTRAINT ck_koth_api_observer_operations_expected_revision
        CHECK (expected_revision BETWEEN 0 AND 9007199254740990),
    CONSTRAINT ck_koth_api_observer_operations_result_revision
        CHECK (
            (completed_at IS NULL AND result_revision IS NULL AND result IS NULL
             AND disclosure_count = 0 AND last_disclosed_at IS NULL)
            OR
            (completed_at IS NOT NULL AND result_revision IS NOT NULL
             AND result_revision = expected_revision + 1
             AND result IS NOT NULL AND jsonb_typeof(result) = 'object'
             AND result ?& ARRAY['operationId', 'challengeId', 'revision']
             AND result->>'operationId' = operation_id::text
             AND (result->>'challengeId')::integer = challenge_id
             AND (result->>'revision')::bigint = result_revision
             AND disclosure_count >= 1 AND last_disclosed_at IS NOT NULL)
        ),
    CONSTRAINT ck_koth_api_observer_operations_disclosures
        CHECK (disclosure_count >= 0),
    CONSTRAINT ck_koth_api_observer_operations_expiry
        CHECK (expires_at > created_at)
);

CREATE INDEX IF NOT EXISTS ix_koth_api_observer_operations_scope
    ON "KothApiObserverOperations"
       (game_id, challenge_id, actor_user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS ix_koth_api_observer_operations_expiry
    ON "KothApiObserverOperations"(expires_at);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "KothApiObserverOperations";
DROP TABLE IF EXISTS "KothApiObserverRevisions";
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
    fn observer_mutations_have_a_revision_fence_and_bounded_recovery_record() {
        assert!(UP_SQL.contains("revision BIGINT NOT NULL DEFAULT 0"));
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("expected_revision BIGINT NOT NULL"));
        assert!(UP_SQL.contains("result JSONB NULL"));
        assert!(UP_SQL.contains("result_revision = expected_revision + 1"));
        assert!(UP_SQL.contains("result IS NOT NULL"));
        assert!(UP_SQL.contains("result ?& ARRAY['operationId', 'challengeId', 'revision']"));
        assert!(UP_SQL.contains("result->>'operationId' = operation_id::text"));
        assert!(UP_SQL.contains("interval '24 hours'"));
        assert!(UP_SQL.contains("disclosure_count INTEGER NOT NULL DEFAULT 0"));
        assert!(UP_SQL.contains("ON DELETE SET NULL"));
        assert!(UP_SQL.contains("ON CONFLICT (challenge_id) DO NOTHING"));
    }
}
