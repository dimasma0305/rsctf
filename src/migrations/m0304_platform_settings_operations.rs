//! Revision fence, idempotency ledger, and durable branding staging for platform settings.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "PlatformSettingsState" (
    singleton SMALLINT PRIMARY KEY DEFAULT 1,
    revision BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT ck_platform_settings_singleton CHECK (singleton = 1),
    CONSTRAINT ck_platform_settings_revision
        CHECK (revision BETWEEN 0 AND 9007199254740991)
);

INSERT INTO "PlatformSettingsState" (singleton, revision)
VALUES (1, 0)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS "PlatformSettingsOperations" (
    operation_id UUID PRIMARY KEY,
    actor_user_id UUID NULL,
    request_digest BYTEA NOT NULL,
    expected_revision BIGINT NOT NULL,
    result_revision BIGINT NOT NULL,
    branding_hash TEXT NULL,
    completed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_platform_settings_operation_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    CONSTRAINT ck_platform_settings_operation_digest
        CHECK (OCTET_LENGTH(request_digest) = 32),
    CONSTRAINT ck_platform_settings_operation_expected_revision
        CHECK (expected_revision BETWEEN 0 AND 9007199254740990),
    CONSTRAINT ck_platform_settings_operation_result_revision
        CHECK (result_revision = expected_revision + 1)
);

CREATE INDEX IF NOT EXISTS ix_platform_settings_operations_retention
    ON "PlatformSettingsOperations"(completed_at, operation_id);

CREATE TABLE IF NOT EXISTS "PlatformSettingsBrandingStaging" (
    operation_id UUID PRIMARY KEY,
    actor_user_id UUID NULL,
    request_digest BYTEA NOT NULL,
    blob_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (clock_timestamp() + interval '24 hours'),
    CONSTRAINT fk_platform_settings_branding_actor
        FOREIGN KEY (actor_user_id) REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
    CONSTRAINT fk_platform_settings_branding_blob
        FOREIGN KEY (blob_hash) REFERENCES "Files"(hash) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_settings_branding_digest
        CHECK (OCTET_LENGTH(request_digest) = 32),
    CONSTRAINT ck_platform_settings_branding_expiry CHECK (expires_at > created_at)
);

CREATE INDEX IF NOT EXISTS ix_platform_settings_branding_expiry
    ON "PlatformSettingsBrandingStaging"(expires_at, operation_id);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "PlatformSettingsBrandingStaging";
DROP TABLE IF EXISTS "PlatformSettingsOperations";
DROP TABLE IF EXISTS "PlatformSettingsState";
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
    fn settings_mutations_are_revisioned_replayable_and_bounded() {
        assert!(UP_SQL.contains("singleton SMALLINT PRIMARY KEY DEFAULT 1"));
        assert!(UP_SQL.contains("revision BIGINT NOT NULL DEFAULT 0"));
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("OCTET_LENGTH(request_digest) = 32"));
        assert!(UP_SQL.contains("result_revision = expected_revision + 1"));
        assert!(UP_SQL.contains("interval '24 hours'"));
        assert!(UP_SQL.contains("ON DELETE SET NULL"));
        assert!(UP_SQL.contains("ix_platform_settings_operations_retention"));
        assert!(UP_SQL.contains("FOREIGN KEY (blob_hash) REFERENCES \"Files\"(hash)"));
        assert!(UP_SQL.contains("ON CONFLICT (singleton) DO NOTHING"));
    }
}
