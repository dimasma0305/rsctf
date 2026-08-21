//! Bound player-controlled profile text at the database boundary.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
UPDATE "Teams"
   SET name = CASE
       WHEN BTRIM(name) = '' THEN 'Team ' || id::text
       ELSE LEFT(BTRIM(name), 128)
   END,
       bio = CASE WHEN bio IS NULL THEN NULL ELSE LEFT(bio, 4096) END;

UPDATE "AspNetUsers"
   SET bio = LEFT(bio, 4096),
       phone_number = CASE
           WHEN phone_number IS NULL THEN NULL ELSE LEFT(phone_number, 64)
       END,
       real_name = LEFT(real_name, 256),
       std_number = LEFT(std_number, 128);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_teams_name_length'
           AND conrelid = '"Teams"'::regclass
    ) THEN
        ALTER TABLE "Teams" ADD CONSTRAINT ck_teams_name_length
            CHECK (CHAR_LENGTH(name) BETWEEN 1 AND 128);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_teams_bio_length'
           AND conrelid = '"Teams"'::regclass
    ) THEN
        ALTER TABLE "Teams" ADD CONSTRAINT ck_teams_bio_length
            CHECK (bio IS NULL OR CHAR_LENGTH(bio) <= 4096);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_users_bio_length'
           AND conrelid = '"AspNetUsers"'::regclass
    ) THEN
        ALTER TABLE "AspNetUsers" ADD CONSTRAINT ck_users_bio_length
            CHECK (CHAR_LENGTH(bio) <= 4096);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_users_phone_length'
           AND conrelid = '"AspNetUsers"'::regclass
    ) THEN
        ALTER TABLE "AspNetUsers" ADD CONSTRAINT ck_users_phone_length
            CHECK (phone_number IS NULL OR CHAR_LENGTH(phone_number) <= 64);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_users_real_name_length'
           AND conrelid = '"AspNetUsers"'::regclass
    ) THEN
        ALTER TABLE "AspNetUsers" ADD CONSTRAINT ck_users_real_name_length
            CHECK (CHAR_LENGTH(real_name) <= 256);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_users_std_number_length'
           AND conrelid = '"AspNetUsers"'::regclass
    ) THEN
        ALTER TABLE "AspNetUsers" ADD CONSTRAINT ck_users_std_number_length
            CHECK (CHAR_LENGTH(std_number) <= 128);
    END IF;
END $$;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "Teams"
    DROP CONSTRAINT IF EXISTS ck_teams_bio_length,
    DROP CONSTRAINT IF EXISTS ck_teams_name_length;
ALTER TABLE "AspNetUsers"
    DROP CONSTRAINT IF EXISTS ck_users_std_number_length,
    DROP CONSTRAINT IF EXISTS ck_users_real_name_length,
    DROP CONSTRAINT IF EXISTS ck_users_phone_length,
    DROP CONSTRAINT IF EXISTS ck_users_bio_length;
"#;

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
    use super::*;

    #[test]
    fn migration_repairs_existing_rows_before_adding_all_bounds() {
        assert!(UP_SQL.starts_with("\nUPDATE \"Teams\""));
        assert!(UP_SQL.contains("LEFT(BTRIM(name), 128)"));
        assert!(UP_SQL.contains("ck_teams_bio_length"));
        assert!(UP_SQL.contains("ck_users_phone_length"));
        assert!(UP_SQL.contains("ck_users_real_name_length"));
        assert!(UP_SQL.contains("ck_users_std_number_length"));
    }
}
