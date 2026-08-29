//! Revision-fenced, short-lived recovery records for player credential mutations.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "PlayerCredentialRevisions" (
    participation_id INTEGER NOT NULL,
    credential_kind VARCHAR(16) NOT NULL,
    challenge_id INTEGER NOT NULL DEFAULT 0,
    revision BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (participation_id, credential_kind, challenge_id),
    CONSTRAINT fk_player_credential_revisions_participation
        FOREIGN KEY (participation_id) REFERENCES "Participations"(id)
        ON DELETE CASCADE,
    CONSTRAINT ck_player_credential_revisions_kind
        CHECK (credential_kind IN ('AdToken', 'AdSsh', 'KothApi')),
    CONSTRAINT ck_player_credential_revisions_challenge
        CHECK ((credential_kind = 'KothApi' AND challenge_id > 0)
            OR (credential_kind <> 'KothApi' AND challenge_id = 0)),
    CONSTRAINT ck_player_credential_revisions_revision
        CHECK (revision BETWEEN 0 AND 9007199254740991)
);

INSERT INTO "PlayerCredentialRevisions"
    (participation_id, credential_kind, challenge_id, revision, updated_at)
SELECT token.participation_id, 'AdToken', 0, 1,
       COALESCE(token.last_rotated_at_utc, token.created_at_utc)
  FROM "AdTeamApiTokens" token
ON CONFLICT DO NOTHING;

INSERT INTO "PlayerCredentialRevisions"
    (participation_id, credential_kind, challenge_id, revision, updated_at)
SELECT key.participation_id, 'AdSsh', 0, 1, key.created_at_utc
  FROM "AdSshKeys" key
ON CONFLICT DO NOTHING;

INSERT INTO "PlayerCredentialRevisions"
    (participation_id, credential_kind, challenge_id, revision, updated_at)
SELECT token.participation_id, 'KothApi', token.challenge_id,
       GREATEST(token.generation::bigint, 1), token.rotated_at
  FROM "KothApiTeamTokens" token
ON CONFLICT DO NOTHING;

CREATE TABLE IF NOT EXISTS "PlayerCredentialOperations" (
    operation_id UUID PRIMARY KEY,
    participation_id INTEGER NOT NULL,
    game_id INTEGER NOT NULL,
    actor_user_id UUID NOT NULL,
    credential_kind VARCHAR(16) NOT NULL,
    challenge_id INTEGER NOT NULL DEFAULT 0,
    expected_revision BIGINT NOT NULL,
    result_revision BIGINT NULL,
    result_ciphertext BYTEA NULL,
    result_nonce BYTEA NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ NULL,
    expires_at TIMESTAMPTZ NOT NULL
        DEFAULT (clock_timestamp() + interval '15 minutes'),
    disclosure_count INTEGER NOT NULL DEFAULT 0,
    last_disclosed_at TIMESTAMPTZ NULL,
    CONSTRAINT fk_player_credential_operations_participation
        FOREIGN KEY (participation_id) REFERENCES "Participations"(id)
        ON DELETE CASCADE,
    CONSTRAINT fk_player_credential_operations_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id)
        ON DELETE CASCADE,
    CONSTRAINT ck_player_credential_operations_kind
        CHECK (credential_kind IN ('AdToken', 'AdSsh', 'KothApi')),
    CONSTRAINT ck_player_credential_operations_challenge
        CHECK ((credential_kind = 'KothApi' AND challenge_id > 0)
            OR (credential_kind <> 'KothApi' AND challenge_id = 0)),
    CONSTRAINT ck_player_credential_operations_expected_revision
        CHECK (expected_revision BETWEEN 0 AND 9007199254740990),
    CONSTRAINT ck_player_credential_operations_result
        CHECK ((completed_at IS NULL AND result_revision IS NULL
                AND result_ciphertext IS NULL AND result_nonce IS NULL
                AND disclosure_count = 0 AND last_disclosed_at IS NULL)
            OR (completed_at IS NOT NULL
                AND result_revision = expected_revision + 1
                AND octet_length(result_ciphertext) BETWEEN 17 AND 262144
                AND octet_length(result_nonce) = 12
                AND disclosure_count >= 1 AND last_disclosed_at IS NOT NULL)),
    CONSTRAINT ck_player_credential_operations_expiry
        CHECK (expires_at > created_at)
);

CREATE INDEX IF NOT EXISTS ix_player_credential_operations_scope
    ON "PlayerCredentialOperations"
       (participation_id, credential_kind, challenge_id, created_at DESC);
CREATE INDEX IF NOT EXISTS ix_player_credential_operations_expiry
    ON "PlayerCredentialOperations"(expires_at);
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
    fn one_time_credentials_have_revision_fences_and_bounded_encrypted_recovery() {
        assert!(UP_SQL.contains("PRIMARY KEY (participation_id, credential_kind, challenge_id)"));
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("expected_revision BIGINT NOT NULL"));
        assert!(UP_SQL.contains("result_revision = expected_revision + 1"));
        assert!(UP_SQL.contains("result_ciphertext BYTEA NULL"));
        assert!(UP_SQL.contains("octet_length(result_nonce) = 12"));
        assert!(UP_SQL.contains("interval '15 minutes'"));
        assert!(UP_SQL.contains("ON CONFLICT DO NOTHING"));
    }
}
