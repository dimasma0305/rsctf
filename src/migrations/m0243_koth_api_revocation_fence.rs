//! Add a durable pending fence for Leaderboard capability revocation and
//! recovery.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'KothApiTeamTokens'
           AND column_name = 'revocation_pending'
    ) THEN
        ALTER TABLE "KothApiTeamTokens"
            ADD COLUMN revocation_pending BOOLEAN NOT NULL DEFAULT FALSE;

        -- Fail closed for an already-ineligible retained row after upgrade.
        -- The column-existence guard makes an idempotent replay a no-op even
        -- after the application has reconciled this initial request.
        UPDATE "KothApiTeamTokens" capability
           SET revocation_pending = TRUE
          FROM "Participations" participation
          JOIN "Teams" team ON team.id = participation.team_id
         WHERE capability.game_id = participation.game_id
           AND capability.participation_id = participation.id
           AND (
               participation.status <> 1
               OR team.deletion_pending
               OR EXISTS (
                   SELECT 1
                     FROM (
                         SELECT team.captain_id AS user_id
                         UNION
                         SELECT member.user_id
                           FROM "TeamMembers" member
                          WHERE member.team_id = team.id
                     ) roster_member
                     LEFT JOIN "AspNetUsers" account
                       ON account.id = roster_member.user_id
                    WHERE account.id IS NULL OR account.role = 0
               )
           );
    END IF;
END
$$;

-- Account role mutations are rare. Every statement that targets the role
-- column takes one fixed exclusive transaction lock. Final Leaderboard
-- checker brackets take its shared form before re-reading live eligibility, so
-- a ban/unban linearizes wholly before or after scoring while checkers remain
-- mutually concurrent.
CREATE OR REPLACE FUNCTION rsctf_fence_koth_role_eligibility()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
    PERFORM pg_advisory_xact_lock(5932159163412923205);
    RETURN NULL;
END
$function$;

DROP TRIGGER IF EXISTS rsctf_koth_role_eligibility_fence
    ON "AspNetUsers";
CREATE TRIGGER rsctf_koth_role_eligibility_fence
AFTER UPDATE OF role ON "AspNetUsers"
FOR EACH STATEMENT
EXECUTE FUNCTION rsctf_fence_koth_role_eligibility();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: older binaries ignore the added fence columns.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn revocation_fence_is_idempotent_and_upgrade_fail_closed() {
        assert!(UP_SQL.contains("column_name = 'revocation_pending'"));
        assert!(UP_SQL.contains("ADD COLUMN revocation_pending"));
        assert!(UP_SQL.contains("participation.status <> 1"));
        assert!(UP_SQL.contains("account.id IS NULL OR account.role = 0"));
        assert!(UP_SQL.contains("AFTER UPDATE OF role ON \"AspNetUsers\""));
        assert!(!UP_SQL.contains("REFERENCING OLD TABLE"));
        assert!(UP_SQL.contains("pg_advisory_xact_lock(5932159163412923205)"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_and_role_fence_are_idempotent_and_linearizable() {
        use sqlx::{Connection as _, Executor as _};

        const LOCK_ID: i64 = 5_932_159_163_412_923_205;
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let schema = format!(
            "m0243_{}",
            crate::utils::codec::random_token(8).to_ascii_lowercase()
        );
        let search_path = format!(r#"SET search_path TO "{schema}""#);
        let mut setup = sqlx::PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(&format!(
            r#"CREATE SCHEMA "{schema}";
               SET search_path TO "{schema}";
               CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY, role SMALLINT NOT NULL,
                 last_visited TIMESTAMPTZ
               );
               CREATE TABLE "Teams" (
                 id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
                 deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
               );
               CREATE TABLE "TeamMembers" (team_id INTEGER, user_id UUID);
               CREATE TABLE "Participations" (
                 id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                 team_id INTEGER NOT NULL, status SMALLINT NOT NULL
               );
               CREATE TABLE "KothApiTeamTokens" (
                 game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
                 participation_id INTEGER NOT NULL, token TEXT NOT NULL
               );
               INSERT INTO "AspNetUsers" VALUES
                 ('00000000-0000-0000-0000-000000000001', 1, NULL),
                 ('00000000-0000-0000-0000-000000000002', 0, NULL),
                 ('00000000-0000-0000-0000-000000000003', 1, NULL);
               INSERT INTO "Teams" VALUES
                 (1, '00000000-0000-0000-0000-000000000001', FALSE),
                 (2, '00000000-0000-0000-0000-000000000001', FALSE),
                 (3, '00000000-0000-0000-0000-000000000002', FALSE),
                 (4, '00000000-0000-0000-0000-000000000004', FALSE),
                 (5, '00000000-0000-0000-0000-000000000003', TRUE);
               INSERT INTO "Participations" VALUES
                 (1, 7, 1, 1), (2, 7, 2, 3), (3, 7, 3, 1),
                 (4, 7, 4, 1), (5, 7, 5, 1);
               INSERT INTO "KothApiTeamTokens" VALUES
                 (7, 9, 1, 'koth_live'),
                 (7, 9, 2, 'koth_suspended'),
                 (7, 9, 3, 'koth_banned'),
                 (7, 9, 4, 'koth_missing'),
                 (7, 9, 5, 'koth_deleting');"#
        ))
        .execute(&mut setup)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&mut setup).await.unwrap();
        let initial_fences: Vec<(i32, bool)> = sqlx::query_as(
            r#"SELECT participation_id, revocation_pending
                 FROM "KothApiTeamTokens" ORDER BY participation_id"#,
        )
        .fetch_all(&mut setup)
        .await
        .unwrap();
        assert_eq!(
            initial_fences,
            vec![(1, false), (2, true), (3, true), (4, true), (5, true)]
        );
        sqlx::query(r#"UPDATE "KothApiTeamTokens" SET revocation_pending = FALSE"#)
            .execute(&mut setup)
            .await
            .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&mut setup).await.unwrap();
        let migrated: (i64, bool) = sqlx::query_as(
            r#"SELECT (
                   SELECT COUNT(*) FROM pg_trigger
                    WHERE tgname = 'rsctf_koth_role_eligibility_fence'
                      AND tgrelid = '"AspNetUsers"'::regclass
                      AND NOT tgisinternal
                 ),
                 (SELECT BOOL_AND(NOT revocation_pending)
                    FROM "KothApiTeamTokens")"#,
        )
        .fetch_one(&mut setup)
        .await
        .unwrap();
        assert_eq!(migrated, (1, true));

        // A row holder blocks the role UPDATE before its AFTER trigger can take
        // the global fence, so unrelated scoring readers remain concurrent.
        let mut row_holder = sqlx::PgConnection::connect(&database_url).await.unwrap();
        row_holder.execute(search_path.as_str()).await.unwrap();
        let mut held = row_holder.begin().await.unwrap();
        sqlx::query(r#"SELECT id FROM "AspNetUsers" WHERE id = $1 FOR SHARE"#)
            .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
            .fetch_one(&mut *held)
            .await
            .unwrap();
        let mut writer = sqlx::PgConnection::connect(&database_url).await.unwrap();
        writer.execute(search_path.as_str()).await.unwrap();
        let mut blocked_writer = tokio::spawn(async move {
            sqlx::query(r#"UPDATE "AspNetUsers" SET role = 0 WHERE id = $1"#)
                .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
                .execute(&mut writer)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut blocked_writer)
                .await
                .is_err()
        );
        let mut probe = sqlx::PgConnection::connect(&database_url).await.unwrap();
        let shared_available: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock_shared($1)")
                .bind(LOCK_ID)
                .fetch_one(&mut probe)
                .await
                .unwrap();
        assert!(shared_available);
        held.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), blocked_writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        // Writer-first makes a later shared checker wait, then its separate
        // READ COMMITTED statement observes the committed role.
        let mut writer = sqlx::PgConnection::connect(&database_url).await.unwrap();
        writer.execute(search_path.as_str()).await.unwrap();
        let mut writer_tx = writer.begin().await.unwrap();
        sqlx::query(r#"UPDATE "AspNetUsers" SET role = 1 WHERE id = $1"#)
            .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
            .execute(&mut *writer_tx)
            .await
            .unwrap();
        let database_url_for_reader = database_url.clone();
        let search_path_for_reader = search_path.clone();
        let mut reader = tokio::spawn(async move {
            let mut connection = sqlx::PgConnection::connect(&database_url_for_reader)
                .await
                .unwrap();
            connection
                .execute(search_path_for_reader.as_str())
                .await
                .unwrap();
            let mut transaction = connection.begin().await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
                .bind(LOCK_ID)
                .execute(&mut *transaction)
                .await
                .unwrap();
            let role: i16 = sqlx::query_scalar(r#"SELECT role FROM "AspNetUsers" WHERE id = $1"#)
                .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
            transaction.rollback().await.unwrap();
            role
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut reader)
                .await
                .is_err()
        );
        writer_tx.commit().await.unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), reader)
                .await
                .unwrap()
                .unwrap(),
            1
        );

        // Checker-first holds the shared fence through its final eligibility
        // read and commit. A role update linearizes after it.
        let mut checker = sqlx::PgConnection::connect(&database_url).await.unwrap();
        checker.execute(search_path.as_str()).await.unwrap();
        let mut checker_tx = checker.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
            .bind(LOCK_ID)
            .execute(&mut *checker_tx)
            .await
            .unwrap();
        let checker_role: i16 =
            sqlx::query_scalar(r#"SELECT role FROM "AspNetUsers" WHERE id = $1"#)
                .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
                .fetch_one(&mut *checker_tx)
                .await
                .unwrap();
        assert_eq!(checker_role, 1);

        // Shared checker finals do not convoy each other.
        let mut second_checker = sqlx::PgConnection::connect(&database_url).await.unwrap();
        second_checker.execute(search_path.as_str()).await.unwrap();
        let mut second_checker_tx = second_checker.begin().await.unwrap();
        let shared_concurrent: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock_shared($1)")
                .bind(LOCK_ID)
                .fetch_one(&mut *second_checker_tx)
                .await
                .unwrap();
        assert!(shared_concurrent);

        let mut role_writer = sqlx::PgConnection::connect(&database_url).await.unwrap();
        role_writer.execute(search_path.as_str()).await.unwrap();
        let mut role_update = tokio::spawn(async move {
            sqlx::query(r#"UPDATE "AspNetUsers" SET role = 0 WHERE id = $1"#)
                .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
                .execute(&mut role_writer)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut role_update)
                .await
                .is_err()
        );

        // An unrelated activity update never invokes the role-only trigger,
        // even while a role writer is queued behind scoring readers.
        let mut activity = sqlx::PgConnection::connect(&database_url).await.unwrap();
        activity.execute(search_path.as_str()).await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sqlx::query(
                r#"UPDATE "AspNetUsers" SET last_visited = clock_timestamp() WHERE id = $1"#,
            )
            .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap())
            .execute(&mut activity),
        )
        .await
        .expect("non-role update waited on the KotH eligibility fence")
        .unwrap();
        second_checker_tx.rollback().await.unwrap();
        checker_tx.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), role_update)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let committed_role: i16 =
            sqlx::query_scalar(r#"SELECT role FROM "AspNetUsers" WHERE id = $1"#)
                .bind(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
                .fetch_one(&mut setup)
                .await
                .unwrap();
        assert_eq!(committed_role, 0);

        setup
            .execute(format!(r#"DROP SCHEMA "{schema}" CASCADE"#).as_str())
            .await
            .unwrap();
    }
}
