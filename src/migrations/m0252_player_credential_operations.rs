//! Durable revision fences and encrypted, short-lived recovery for player credentials.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
-- Hold one rollout fence from the legacy-table backfill through trigger
-- installation. Old replicas may keep reading, but no semantic writer can
-- slip between the snapshot and the compatibility triggers.
LOCK TABLE "AdTeamApiTokens", "AdSshKeys", "KothApiTeamTokens"
    IN SHARE ROW EXCLUSIVE MODE;

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
ON CONFLICT (participation_id, credential_kind, challenge_id) DO UPDATE
SET revision = GREATEST("PlayerCredentialRevisions".revision, EXCLUDED.revision),
    updated_at = GREATEST("PlayerCredentialRevisions".updated_at, EXCLUDED.updated_at);

INSERT INTO "PlayerCredentialRevisions"
    (participation_id, credential_kind, challenge_id, revision, updated_at)
SELECT key.participation_id, 'AdSsh', 0, 1, key.created_at_utc
  FROM "AdSshKeys" key
ON CONFLICT (participation_id, credential_kind, challenge_id) DO UPDATE
SET revision = GREATEST("PlayerCredentialRevisions".revision, EXCLUDED.revision),
    updated_at = GREATEST("PlayerCredentialRevisions".updated_at, EXCLUDED.updated_at);

INSERT INTO "PlayerCredentialRevisions"
    (participation_id, credential_kind, challenge_id, revision, updated_at)
SELECT token.participation_id, 'KothApi', token.challenge_id,
       GREATEST(token.generation::bigint, 1), token.rotated_at
  FROM "KothApiTeamTokens" token
ON CONFLICT (participation_id, credential_kind, challenge_id) DO UPDATE
SET revision = GREATEST("PlayerCredentialRevisions".revision, EXCLUDED.revision),
    updated_at = GREATEST("PlayerCredentialRevisions".updated_at, EXCLUDED.updated_at);

CREATE TABLE IF NOT EXISTS "PlayerCredentialOperations" (
    operation_id UUID PRIMARY KEY,
    participation_id INTEGER NOT NULL,
    game_id INTEGER NOT NULL,
    actor_user_id UUID NOT NULL,
    credential_kind VARCHAR(16) NOT NULL,
    challenge_id INTEGER NOT NULL DEFAULT 0,
    expected_revision BIGINT NOT NULL,
    request_hash BYTEA NOT NULL,
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
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE CASCADE,
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
    CONSTRAINT ck_player_credential_operations_request_hash
        CHECK (octet_length(request_hash) = 32),
    CONSTRAINT ck_player_credential_operations_result
        CHECK ((completed_at IS NULL AND result_revision IS NULL
                AND result_ciphertext IS NULL AND result_nonce IS NULL
                AND disclosure_count = 0 AND last_disclosed_at IS NULL)
            OR (completed_at IS NOT NULL
                AND result_revision = expected_revision + 1
                AND octet_length(result_ciphertext) BETWEEN 17 AND 65552
                AND octet_length(result_nonce) = 12
                AND disclosure_count >= 1 AND last_disclosed_at IS NOT NULL)),
    CONSTRAINT ck_player_credential_operations_expiry
        CHECK (expires_at > created_at)
);

-- An unreleased prototype used the same table name without a request hash and
-- with a participation-only foreign key. Its ciphertext uses incompatible AAD,
-- so never silently discard a still-recoverable result during an upgrade.
ALTER TABLE "PlayerCredentialOperations"
    ADD COLUMN IF NOT EXISTS request_hash BYTEA NULL;

DELETE FROM "PlayerCredentialOperations"
 WHERE request_hash IS NULL AND expires_at <= clock_timestamp();

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM "PlayerCredentialOperations" WHERE request_hash IS NULL
    ) THEN
        RAISE EXCEPTION
            'legacy player credential recovery operations are still active'
            USING ERRCODE = '55000',
                  HINT = 'Retry the migration after their 15-minute recovery window expires';
    END IF;
END
$$;

ALTER TABLE "PlayerCredentialOperations"
    ALTER COLUMN request_hash SET NOT NULL,
    DROP CONSTRAINT IF EXISTS fk_player_credential_operations_participation,
    DROP CONSTRAINT IF EXISTS ck_player_credential_operations_request_hash,
    DROP CONSTRAINT IF EXISTS ck_player_credential_operations_result;

ALTER TABLE "PlayerCredentialOperations"
    ADD CONSTRAINT fk_player_credential_operations_participation
        FOREIGN KEY (game_id, participation_id)
        REFERENCES "Participations"(game_id, id) ON DELETE CASCADE,
    ADD CONSTRAINT ck_player_credential_operations_request_hash
        CHECK (octet_length(request_hash) = 32),
    ADD CONSTRAINT ck_player_credential_operations_result
        CHECK ((completed_at IS NULL AND result_revision IS NULL
                AND result_ciphertext IS NULL AND result_nonce IS NULL
                AND disclosure_count = 0 AND last_disclosed_at IS NULL)
            OR (completed_at IS NOT NULL
                AND result_revision = expected_revision + 1
                AND octet_length(result_ciphertext) BETWEEN 17 AND 65552
                AND octet_length(result_nonce) = 12
                AND disclosure_count >= 1 AND last_disclosed_at IS NOT NULL));

-- Compatibility writers (including an older replica during a rolling
-- deployment) do not know about PlayerCredentialRevisions. Advance the same
-- fence in PostgreSQL for every semantic credential change. New revision-CAS
-- handlers mark their exact scope transaction-locally and advance it once in
-- complete(), so their table write is not counted twice.
CREATE OR REPLACE FUNCTION rsctf_bump_player_credential_revision()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    target_participation INTEGER;
    target_challenge INTEGER := 0;
    target_kind TEXT := TG_ARGV[0];
    generation_floor BIGINT := 1;
    managed_scope TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_participation := OLD.participation_id;
        IF target_kind = 'KothApi' THEN
            target_challenge := OLD.challenge_id;
            generation_floor := GREATEST(OLD.generation::BIGINT, 1);
        END IF;
    ELSE
        target_participation := NEW.participation_id;
        IF target_kind = 'KothApi' THEN
            target_challenge := NEW.challenge_id;
            generation_floor := GREATEST(NEW.generation::BIGINT, 1);
        END IF;
    END IF;

    managed_scope := format(
        '%s:%s:%s', target_participation, target_kind, target_challenge
    );
    IF NULLIF(
        current_setting('rsctf.player_credential_revision_managed', TRUE), ''
    ) = managed_scope THEN
        RETURN NULL;
    END IF;

    -- INSERT ... SELECT is cascade-safe: deleting a participation cannot make
    -- its child DELETE trigger recreate a revision row with a dangling FK.
    INSERT INTO "PlayerCredentialRevisions"
        (participation_id, credential_kind, challenge_id, revision, updated_at)
    SELECT
        target_participation, target_kind, target_challenge,
        generation_floor, clock_timestamp()
      FROM "Participations" participation
     WHERE participation.id = target_participation
    ON CONFLICT (participation_id, credential_kind, challenge_id) DO UPDATE
    SET revision = GREATEST(
            "PlayerCredentialRevisions".revision + 1,
            EXCLUDED.revision
        ),
        updated_at = clock_timestamp();
    RETURN NULL;
END
$$;

DROP TRIGGER IF EXISTS tr_player_credential_ad_token_insert_delete
    ON "AdTeamApiTokens";
CREATE TRIGGER tr_player_credential_ad_token_insert_delete
AFTER INSERT OR DELETE ON "AdTeamApiTokens"
FOR EACH ROW EXECUTE FUNCTION rsctf_bump_player_credential_revision('AdToken');
DROP TRIGGER IF EXISTS tr_player_credential_ad_token_update
    ON "AdTeamApiTokens";
CREATE TRIGGER tr_player_credential_ad_token_update
AFTER UPDATE OF token_hash, hint, created_at_utc, last_rotated_at_utc
ON "AdTeamApiTokens"
FOR EACH ROW
WHEN ((NEW.token_hash, NEW.hint, NEW.created_at_utc, NEW.last_rotated_at_utc)
      IS DISTINCT FROM
      (OLD.token_hash, OLD.hint, OLD.created_at_utc, OLD.last_rotated_at_utc))
EXECUTE FUNCTION rsctf_bump_player_credential_revision('AdToken');

DROP TRIGGER IF EXISTS tr_player_credential_ad_ssh_insert_delete
    ON "AdSshKeys";
CREATE TRIGGER tr_player_credential_ad_ssh_insert_delete
AFTER INSERT OR DELETE ON "AdSshKeys"
FOR EACH ROW EXECUTE FUNCTION rsctf_bump_player_credential_revision('AdSsh');
DROP TRIGGER IF EXISTS tr_player_credential_ad_ssh_update
    ON "AdSshKeys";
CREATE TRIGGER tr_player_credential_ad_ssh_update
AFTER UPDATE OF algorithm, public_key, fingerprint, platform_generated, created_at_utc
ON "AdSshKeys"
FOR EACH ROW
WHEN ((NEW.algorithm, NEW.public_key, NEW.fingerprint,
       NEW.platform_generated, NEW.created_at_utc)
      IS DISTINCT FROM
      (OLD.algorithm, OLD.public_key, OLD.fingerprint,
       OLD.platform_generated, OLD.created_at_utc))
EXECUTE FUNCTION rsctf_bump_player_credential_revision('AdSsh');

DROP TRIGGER IF EXISTS tr_player_credential_koth_api_insert_delete
    ON "KothApiTeamTokens";
CREATE TRIGGER tr_player_credential_koth_api_insert_delete
AFTER INSERT OR DELETE ON "KothApiTeamTokens"
FOR EACH ROW EXECUTE FUNCTION rsctf_bump_player_credential_revision('KothApi');
DROP TRIGGER IF EXISTS tr_player_credential_koth_api_update
    ON "KothApiTeamTokens";
CREATE TRIGGER tr_player_credential_koth_api_update
AFTER UPDATE OF token, generation ON "KothApiTeamTokens"
FOR EACH ROW
WHEN ((NEW.token, NEW.generation)
      IS DISTINCT FROM (OLD.token, OLD.generation))
EXECUTE FUNCTION rsctf_bump_player_credential_revision('KothApi');

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
        // Credential revisions are monotonic security state. Rolling this
        // migration back must not erase fences or encrypted recovery records.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::UP_SQL;

    #[test]
    fn one_time_credentials_have_durable_fences_and_encrypted_recovery() {
        assert!(UP_SQL.contains("PRIMARY KEY (participation_id, credential_kind, challenge_id)"));
        assert!(UP_SQL.contains("IN SHARE ROW EXCLUSIVE MODE"));
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("FOREIGN KEY (game_id, participation_id)"));
        assert!(UP_SQL.contains("request_hash BYTEA NOT NULL"));
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS request_hash BYTEA NULL"));
        assert!(UP_SQL.contains("legacy player credential recovery operations are still active"));
        assert!(UP_SQL.contains("ALTER COLUMN request_hash SET NOT NULL"));
        assert!(UP_SQL.contains("octet_length(request_hash) = 32"));
        assert!(UP_SQL.contains("result_revision = expected_revision + 1"));
        assert!(UP_SQL.contains("result_ciphertext BYTEA NULL"));
        assert!(UP_SQL.contains("octet_length(result_nonce) = 12"));
        assert!(UP_SQL.contains("interval '15 minutes'"));
        assert!(UP_SQL.contains("GREATEST(\"PlayerCredentialRevisions\".revision"));
        assert!(UP_SQL.contains("rsctf.player_credential_revision_managed"));
        for trigger in [
            "tr_player_credential_ad_token_insert_delete",
            "tr_player_credential_ad_token_update",
            "tr_player_credential_ad_ssh_insert_delete",
            "tr_player_credential_ad_ssh_update",
            "tr_player_credential_koth_api_insert_delete",
            "tr_player_credential_koth_api_update",
        ] {
            assert!(UP_SQL.contains(trigger));
        }
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_converges_revision_triggers_for_legacy_writers() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0252_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Participations" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                 UNIQUE (game_id, id)
               );
               CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "AdTeamApiTokens" (
                 id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                 participation_id INTEGER NOT NULL UNIQUE
                   REFERENCES "Participations"(id) ON DELETE CASCADE,
                 token_hash TEXT NOT NULL, hint TEXT NOT NULL,
                 created_at_utc TIMESTAMPTZ NOT NULL,
                 last_rotated_at_utc TIMESTAMPTZ,
                 last_used_at_utc TIMESTAMPTZ
               );
               CREATE TABLE "AdSshKeys" (
                 id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                 participation_id INTEGER NOT NULL UNIQUE
                   REFERENCES "Participations"(id) ON DELETE CASCADE,
                 algorithm TEXT NOT NULL, public_key TEXT NOT NULL,
                 fingerprint TEXT NOT NULL, platform_generated BOOLEAN NOT NULL,
                 created_at_utc TIMESTAMPTZ NOT NULL,
                 last_used_at_utc TIMESTAMPTZ
               );
               CREATE TABLE "KothApiTeamTokens" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 participation_id INTEGER NOT NULL,
                 token TEXT NOT NULL, generation INTEGER NOT NULL,
                 created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 last_used_at TIMESTAMPTZ,
                 PRIMARY KEY (game_id, challenge_id, participation_id),
                 FOREIGN KEY (game_id, participation_id)
                   REFERENCES "Participations"(game_id, id) ON DELETE CASCADE
               );
               INSERT INTO "Participations" (id, game_id) VALUES (1, 7);"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        async fn revision(pool: &sqlx::PgPool, kind: &str, challenge_id: i32) -> i64 {
            sqlx::query_scalar(
                r#"SELECT revision FROM "PlayerCredentialRevisions"
                    WHERE participation_id = 1 AND credential_kind = $1
                      AND challenge_id = $2"#,
            )
            .bind(kind)
            .bind(challenge_id)
            .fetch_one(pool)
            .await
            .unwrap()
        }

        sqlx::query(
            r#"INSERT INTO "AdTeamApiTokens"
                   (participation_id, token_hash, hint, created_at_utc)
               VALUES (1, 'hash-a', 'hint-a', clock_timestamp())"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "AdToken", 0).await, 1);
        sqlx::query(
            r#"UPDATE "AdTeamApiTokens" SET last_used_at_utc = clock_timestamp()
                WHERE participation_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "AdToken", 0).await, 1);
        sqlx::query(
            r#"UPDATE "AdTeamApiTokens" SET token_hash = 'hash-b'
                WHERE participation_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "AdToken", 0).await, 2);

        let mut managed = pool.begin().await.unwrap();
        sqlx::query_scalar::<_, String>(
            "SELECT set_config('rsctf.player_credential_revision_managed', '1:AdToken:0', TRUE)",
        )
        .fetch_one(&mut *managed)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "AdTeamApiTokens" SET token_hash = 'hash-managed'
                WHERE participation_id = 1"#,
        )
        .execute(&mut *managed)
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT revision FROM "PlayerCredentialRevisions"
                    WHERE participation_id = 1 AND credential_kind = 'AdToken'
                      AND challenge_id = 0"#,
            )
            .fetch_one(&mut *managed)
            .await
            .unwrap(),
            2
        );
        sqlx::query(
            r#"UPDATE "PlayerCredentialRevisions" SET revision = 3
                WHERE participation_id = 1 AND credential_kind = 'AdToken'
                  AND challenge_id = 0 AND revision = 2"#,
        )
        .execute(&mut *managed)
        .await
        .unwrap();
        managed.commit().await.unwrap();
        assert_eq!(revision(&pool, "AdToken", 0).await, 3);

        sqlx::query(
            r#"INSERT INTO "AdSshKeys"
                   (participation_id, algorithm, public_key, fingerprint,
                    platform_generated, created_at_utc)
               VALUES (1, 'ssh-ed25519', 'key-a', 'fp-a', FALSE,
                       clock_timestamp())"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "AdSsh", 0).await, 1);
        sqlx::query(
            r#"UPDATE "AdSshKeys" SET last_used_at_utc = clock_timestamp()
                WHERE participation_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "AdSsh", 0).await, 1);
        sqlx::query(
            r#"UPDATE "AdSshKeys" SET fingerprint = 'fp-b'
                WHERE participation_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "AdSsh", 0).await, 2);

        sqlx::query(
            r#"INSERT INTO "KothApiTeamTokens"
                   (game_id, challenge_id, participation_id, token, generation)
               VALUES (7, 9, 1, 'koth_token_a', 7)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "KothApi", 9).await, 7);
        sqlx::query(
            r#"UPDATE "KothApiTeamTokens" SET last_used_at = clock_timestamp()
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "KothApi", 9).await, 7);
        sqlx::query(
            r#"UPDATE "KothApiTeamTokens" SET token = 'koth_token_b'
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "KothApi", 9).await, 8);
        sqlx::query(
            r#"DELETE FROM "KothApiTeamTokens"
                WHERE game_id = 7 AND challenge_id = 9 AND participation_id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "KothApi", 9).await, 9);
        sqlx::query(
            r#"INSERT INTO "KothApiTeamTokens"
                   (game_id, challenge_id, participation_id, token, generation)
               VALUES (7, 9, 1, 'koth_token_c', 1)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(revision(&pool, "KothApi", 9).await, 10);

        sqlx::query(r#"DELETE FROM "Participations" WHERE id = 1"#)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*)::BIGINT FROM "PlayerCredentialRevisions""#,
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "cascading credential deletes must not recreate revision rows"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
