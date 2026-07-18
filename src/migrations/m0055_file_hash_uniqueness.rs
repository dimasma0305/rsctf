//! Make content hashes the durable identity of ref-counted file metadata.
//!
//! Older write paths used a read-then-insert sequence, so two replicas could
//! create multiple `Files` rows for the same content hash.  Repoint every
//! relational reference to the oldest row before removing duplicates, carry
//! forward all recorded references, and install the unique index required by
//! atomic `INSERT .. ON CONFLICT` writes.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    LOCK TABLE "Files", "Attachments", "Participations", "AspNetUsers", "Teams",
               "Games", "Configs", "GameChallenges"
      IN SHARE ROW EXCLUSIVE MODE;

    CREATE TEMP TABLE rsctf_file_canonical ON COMMIT DROP AS
    SELECT hash,
           MIN(id) AS canonical_id,
           GREATEST(SUM(GREATEST(reference_count, 0)), 1)::bigint AS carried_references
      FROM "Files"
     GROUP BY hash;

    UPDATE "Attachments" attachment
       SET local_file_id = canonical.canonical_id
      FROM "Files" file
      JOIN rsctf_file_canonical canonical ON canonical.hash = file.hash
     WHERE attachment.local_file_id = file.id
       AND attachment.local_file_id <> canonical.canonical_id;

    UPDATE "Participations" participation
       SET writeup_id = canonical.canonical_id
      FROM "Files" file
      JOIN rsctf_file_canonical canonical ON canonical.hash = file.hash
     WHERE participation.writeup_id = file.id
       AND participation.writeup_id <> canonical.canonical_id;

    UPDATE "Files" file
       SET reference_count = GREATEST(
             canonical.carried_references,
             (SELECT COUNT(*)::bigint
                FROM "Attachments" attachment
               WHERE attachment.local_file_id = canonical.canonical_id)
             +
             (SELECT COUNT(*)::bigint
                FROM "Participations" participation
               WHERE participation.writeup_id = canonical.canonical_id)
             +
             (SELECT COUNT(*)::bigint FROM "AspNetUsers" WHERE avatar_hash = canonical.hash)
             +
             (SELECT COUNT(*)::bigint FROM "Teams" WHERE avatar_hash = canonical.hash)
             +
             (SELECT COUNT(*)::bigint FROM "Games" WHERE poster_hash = canonical.hash)
             +
             (SELECT COUNT(*)::bigint
                FROM "GameChallenges"
               WHERE original_archive_blob_path = canonical.hash)
             +
             CASE WHEN EXISTS (
                  SELECT 1 FROM "Configs"
                   WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
                     AND value = canonical.hash
             ) THEN 1 ELSE 0 END,
             1
           )
      FROM rsctf_file_canonical canonical
     WHERE file.id = canonical.canonical_id;

    DELETE FROM "Files" duplicate
     USING rsctf_file_canonical canonical
     WHERE duplicate.hash = canonical.hash
       AND duplicate.id <> canonical.canonical_id;

    CREATE UNIQUE INDEX IF NOT EXISTS ux_files_hash ON "Files"(hash);
"#;

const DOWN_SQL: &str = r#"
    DROP INDEX IF EXISTS ux_files_hash;
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
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn repoints_both_foreign_keys_before_deduplicating_and_indexing() {
        let attachment_update = UP_SQL.find("UPDATE \"Attachments\"").unwrap();
        let writeup_update = UP_SQL.find("UPDATE \"Participations\"").unwrap();
        let duplicate_delete = UP_SQL.find("DELETE FROM \"Files\" duplicate").unwrap();
        let unique_index = UP_SQL.find("CREATE UNIQUE INDEX").unwrap();

        assert!(attachment_update < duplicate_delete);
        assert!(writeup_update < duplicate_delete);
        assert!(duplicate_delete < unique_index);
        assert!(UP_SQL.contains("SUM(GREATEST(reference_count, 0))"));
        assert!(UP_SQL.contains("attachment.local_file_id = canonical.canonical_id"));
        assert!(UP_SQL.contains("participation.writeup_id = canonical.canonical_id"));
        assert!(UP_SQL.contains("ux_files_hash ON \"Files\"(hash)"));
        assert!(UP_SQL.contains("FROM \"AspNetUsers\" WHERE avatar_hash = canonical.hash"));
        assert!(UP_SQL.contains("FROM \"Teams\" WHERE avatar_hash = canonical.hash"));
        assert!(UP_SQL.contains("FROM \"Games\" WHERE poster_hash = canonical.hash"));
        assert!(UP_SQL.contains("original_archive_blob_path = canonical.hash"));
        assert!(UP_SQL.contains("'GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash'"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn deduplicates_existing_rows_without_dangling_references() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("file_migration_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."Files" (
                id INTEGER PRIMARY KEY,
                hash TEXT NOT NULL,
                reference_count BIGINT NOT NULL
            );
            CREATE TABLE "{schema}"."Attachments" (
                id INTEGER PRIMARY KEY,
                local_file_id INTEGER
            );
            CREATE TABLE "{schema}"."Participations" (
                id INTEGER PRIMARY KEY,
                writeup_id INTEGER
            );
            CREATE TABLE "{schema}"."AspNetUsers" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Teams" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Games" (id INTEGER PRIMARY KEY, poster_hash TEXT);
            CREATE TABLE "{schema}"."Configs" (config_key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE "{schema}"."GameChallenges" (
                id INTEGER PRIMARY KEY,
                original_archive_blob_path TEXT
            );
            INSERT INTO "{schema}"."Files" VALUES
                (10, 'same', 1), (11, 'same', 2), (20, 'orphan', 0);
            INSERT INTO "{schema}"."Attachments" VALUES (1, 11), (2, 11);
            INSERT INTO "{schema}"."Participations" VALUES (1, 11);
            "#
        );
        sqlx::raw_sql(&setup)
            .execute(&admin)
            .await
            .expect("create isolated migration schema");

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect isolated migration pool");
        let mut transaction = pool.begin().await.unwrap();
        sqlx::raw_sql(UP_SQL)
            .execute(&mut *transaction)
            .await
            .expect("apply Files hash migration");
        transaction.commit().await.unwrap();

        let files = sqlx::query_as::<_, (i32, String, i64)>(
            r#"SELECT id, hash, reference_count FROM "Files" ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            files,
            vec![(10, "same".to_string(), 3), (20, "orphan".to_string(), 1)]
        );
        let attachment_ids: Vec<i32> =
            sqlx::query_scalar(r#"SELECT local_file_id FROM "Attachments" ORDER BY id"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(attachment_ids, vec![10, 10]);
        let writeup_id: i32 = sqlx::query_scalar(r#"SELECT writeup_id FROM "Participations""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(writeup_id, 10);
        let duplicate = sqlx::query(
            r#"INSERT INTO "Files" (id, hash, reference_count) VALUES (12, 'same', 1)"#,
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(matches!(
            duplicate,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23505")
        ));

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup).execute(&admin).await.unwrap();
    }
}
