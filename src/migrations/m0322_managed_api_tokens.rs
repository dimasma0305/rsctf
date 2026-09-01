use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
ALTER TABLE "ApiTokens" ADD COLUMN IF NOT EXISTS token_version SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE "ApiTokens" ADD COLUMN IF NOT EXISTS audience TEXT NOT NULL DEFAULT 'legacy-disabled';
ALTER TABLE "ApiTokens" ADD COLUMN IF NOT EXISTS scopes TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[];
ALTER TABLE "ApiTokens" ADD COLUMN IF NOT EXISTS owner_security_stamp_digest BYTEA;
ALTER TABLE "ApiTokens" ALTER COLUMN token_version SET DEFAULT 0;
ALTER TABLE "ApiTokens" ALTER COLUMN audience SET DEFAULT 'legacy-disabled';
ALTER TABLE "ApiTokens" ALTER COLUMN scopes SET DEFAULT ARRAY[]::TEXT[];

UPDATE "ApiTokens"
   SET is_revoked = TRUE,
       token_hash = 'legacy-disabled:' || id::TEXT,
       audience = 'legacy-disabled',
       scopes = ARRAY[]::TEXT[],
       owner_security_stamp_digest = NULL
 WHERE token_version = 0;

DO $$ BEGIN
    ALTER TABLE "ApiTokens" ADD CONSTRAINT ck_api_tokens_version
        CHECK (token_version IN (0, 1));
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
DO $$ BEGIN
    ALTER TABLE "ApiTokens" ADD CONSTRAINT ck_api_tokens_owner_stamp
        CHECK (owner_security_stamp_digest IS NULL OR octet_length(owner_security_stamp_digest) = 32);
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
DO $$ BEGIN
    ALTER TABLE "ApiTokens" ADD CONSTRAINT ck_api_tokens_v1_contract
        CHECK (token_version <> 1 OR (
            audience = 'rsctf-api'
            AND owner_security_stamp_digest IS NOT NULL
            AND cardinality(scopes) BETWEEN 1 AND 2
            AND array_lower(scopes, 1) = 1
            AND array_position(scopes, NULL) IS NULL
            AND scopes::TEXT[] <@ ARRAY['api:read', 'api:write']::TEXT[]
            AND (cardinality(scopes) = 1 OR scopes[1] <> scopes[2])
        ));
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS ux_api_tokens_token_hash ON "ApiTokens" (token_hash);
CREATE INDEX IF NOT EXISTS ix_api_tokens_creator_created
    ON "ApiTokens" (creator_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_api_tokens_creator_active
    ON "ApiTokens" (creator_id, expires_at)
    WHERE token_version = 1 AND NOT is_revoked;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_api_tokens_creator_active;
DROP INDEX IF EXISTS ix_api_tokens_creator_created;
DROP INDEX IF EXISTS ux_api_tokens_token_hash;
ALTER TABLE "ApiTokens" DROP CONSTRAINT IF EXISTS ck_api_tokens_v1_contract;
ALTER TABLE "ApiTokens" DROP CONSTRAINT IF EXISTS ck_api_tokens_owner_stamp;
ALTER TABLE "ApiTokens" DROP CONSTRAINT IF EXISTS ck_api_tokens_version;
ALTER TABLE "ApiTokens" DROP COLUMN IF EXISTS owner_security_stamp_digest;
ALTER TABLE "ApiTokens" DROP COLUMN IF EXISTS scopes;
ALTER TABLE "ApiTokens" DROP COLUMN IF EXISTS audience;
ALTER TABLE "ApiTokens" DROP COLUMN IF EXISTS token_version;
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
    fn legacy_tokens_are_disabled_and_v1_tokens_are_fenced() {
        assert!(UP_SQL.contains("WHERE token_version = 0"));
        assert!(UP_SQL.contains("SET is_revoked = TRUE"));
        assert!(UP_SQL.contains("token_hash = 'legacy-disabled:' || id::TEXT"));
        assert!(UP_SQL.contains("owner_security_stamp_digest"));
        assert!(UP_SQL.contains("ux_api_tokens_token_hash"));
        assert!(UP_SQL.contains("ix_api_tokens_creator_active"));
        assert!(UP_SQL.contains("api:read"));
        assert!(UP_SQL.contains("api:write"));
        assert!(UP_SQL.contains("scopes::TEXT[] <@"));
    }
}
