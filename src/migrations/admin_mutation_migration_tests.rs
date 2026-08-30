use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn admin_mutation_migrations_preserve_legacy_rows_and_enforce_operation_identity() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_admin_mutations_{}", uuid::Uuid::new_v4().simple());
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
        r#"
        CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameChallenges" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL REFERENCES "Games"(id),
            is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
            deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
            review_status SMALLINT NOT NULL DEFAULT 1,
            "Type" SMALLINT NOT NULL DEFAULT 0,
            ad_self_hosted BOOLEAN NOT NULL DEFAULT FALSE,
            flag_template TEXT NULL
        );
        CREATE TABLE "FlagContexts" (
            id SERIAL PRIMARY KEY,
            challenge_id INTEGER NULL REFERENCES "GameChallenges"(id),
            flag TEXT NOT NULL
        );
        CREATE TABLE "Divisions" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY);
        CREATE TABLE "AdTeamServices" (
            id INTEGER PRIMARY KEY,
            challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id)
        );
        CREATE TABLE "AdFlags" (
            id INTEGER PRIMARY KEY,
            team_service_id INTEGER NOT NULL REFERENCES "AdTeamServices"(id),
            flag TEXT NOT NULL
        );
        CREATE TABLE "ChallengeVariants" (
            id UUID PRIMARY KEY,
            challenge_id INTEGER NOT NULL REFERENCES "GameChallenges"(id),
            manifest JSONB NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::raw_sql(super::m0303_mail_outbox::UP_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(super::m0306_bulk_challenge_mutations::UP_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(super::m0307_division_revision_operations::UP_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(super::m0308_team_invite_rotation::UP_SQL)
        .execute(&pool)
        .await
        .unwrap();

    let actor = uuid::Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1)"#)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let first_mail_operation = uuid::Uuid::new_v4();
    let second_mail_operation = uuid::Uuid::new_v4();
    let mail_digest = vec![3_u8; 32];
    for operation_id in [first_mail_operation, second_mail_operation] {
        sqlx::query(
            r#"INSERT INTO "MailOutbox"
                 (operation_id, purpose, account_id, security_generation_digest,
                  destination, destination_digest, request_digest, subject, html_body)
               VALUES ($1, 2, $2, $3, 'new@example.test', $3, $3, 'Change', 'body')"#,
        )
        .bind(operation_id)
        .bind(actor)
        .bind(&mail_digest)
        .execute(&pool)
        .await
        .unwrap();
        if operation_id == first_mail_operation {
            sqlx::query(
                r#"UPDATE "MailOutbox" SET superseded_at_utc = clock_timestamp()
                    WHERE operation_id = $1"#,
            )
            .bind(operation_id)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    sqlx::query(
        r#"INSERT INTO "EmailChangeTickets"
             (operation_id, token_hash, account_id, security_stamp, new_email,
              normalized_email, expires_at_utc, superseded_at_utc)
           VALUES ($1, $2, $3, 'stamp', 'old@example.test',
                   'OLD@EXAMPLE.TEST', clock_timestamp() + INTERVAL '15 minutes',
                   clock_timestamp())"#,
    )
    .bind(first_mail_operation)
    .bind(vec![4_u8; 32])
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "EmailChangeTickets"
             (operation_id, token_hash, account_id, security_stamp, new_email,
              normalized_email, expires_at_utc)
           VALUES ($1, $2, $3, 'stamp', 'new@example.test',
                   'NEW@EXAMPLE.TEST', clock_timestamp() + INTERVAL '15 minutes')"#,
    )
    .bind(second_mail_operation)
    .bind(vec![5_u8; 32])
    .bind(actor)
    .execute(&pool)
    .await
    .expect("a superseded email-change ticket permits one new current link");
    let third_mail_operation = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "MailOutbox"
             (operation_id, purpose, account_id, security_generation_digest,
              destination, destination_digest, request_digest, subject, html_body,
              superseded_at_utc)
           VALUES ($1, 2, $2, $3, 'third@example.test', $3, $3, 'Change',
                   'body', clock_timestamp())"#,
    )
    .bind(third_mail_operation)
    .bind(actor)
    .bind(&mail_digest)
    .execute(&pool)
    .await
    .unwrap();
    let duplicate_current_ticket = sqlx::query(
        r#"INSERT INTO "EmailChangeTickets"
             (operation_id, token_hash, account_id, security_stamp, new_email,
              normalized_email, expires_at_utc)
           VALUES ($1, $2, $3, 'stamp', 'third@example.test',
                   'THIRD@EXAMPLE.TEST', clock_timestamp() + INTERVAL '15 minutes')"#,
    )
    .bind(third_mail_operation)
    .bind(vec![6_u8; 32])
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        duplicate_current_ticket
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505"),
        "one account may expose only one current email-change ticket"
    );
    sqlx::query(r#"INSERT INTO "Games" (id) VALUES (1)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameChallenges" (id, game_id, "Type")
           VALUES (10, 1, 0), (11, 1, 0)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameChallenges" (id, game_id, "Type", flag_template)
           VALUES (12, 1, 3, '')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "FlagContexts" (challenge_id, flag)
           VALUES (10, 'flag{same}'), (10, 'flag{same}'), (11, $1)"#,
    )
    .bind("x".repeat(128))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Divisions" (id) VALUES (20)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id) VALUES (30)"#)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::raw_sql(super::m0309_flag_import_operations::UP_SQL)
        .execute(&pool)
        .await
        .unwrap();

    let duplicate_rows: (i64, i64) = sqlx::query_as(
        r#"SELECT COUNT(*)::bigint,
                  COUNT(*) FILTER (WHERE canonical_identity_enforced)::bigint
             FROM "FlagContexts"
            WHERE challenge_id = 10 AND flag = 'flag{same}'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        duplicate_rows,
        (2, 1),
        "legacy duplicate flags were deleted"
    );
    let invalid_challenge_enabled: bool =
        sqlx::query_scalar(r#"SELECT is_enabled FROM "GameChallenges" WHERE id = 11"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!invalid_challenge_enabled);
    let default_template: (bool, Option<String>) =
        sqlx::query_as(r#"SELECT is_enabled, flag_template FROM "GameChallenges" WHERE id = 12"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(default_template, (true, None));
    sqlx::query(
        r#"INSERT INTO "GameChallenges" (id, game_id, "Type", flag_template)
           VALUES (13, 1, 0, '')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET "Type" = 3 WHERE id = 13"#)
        .execute(&pool)
        .await
        .expect("the runtime default empty template must survive a type transition");
    sqlx::query(
        r#"INSERT INTO "GameChallenges" (id, game_id, "Type", flag_template)
           VALUES (14, 1, 0, ' ')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let invalid_transition = sqlx::query(r#"UPDATE "GameChallenges" SET "Type" = 3 WHERE id = 14"#)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        invalid_transition
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23514")
    );

    let duplicate_flag =
        sqlx::query(r#"INSERT INTO "FlagContexts" (challenge_id, flag) VALUES (10, 'flag{same}')"#)
            .execute(&pool)
            .await
            .unwrap_err();
    assert_eq!(
        duplicate_flag
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    let oversized_flag =
        sqlx::query(r#"INSERT INTO "FlagContexts" (challenge_id, flag) VALUES (10, $1)"#)
            .bind("y".repeat(128))
            .execute(&pool)
            .await
            .unwrap_err();
    assert_eq!(
        oversized_flag
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23514")
    );

    let digest = vec![0_u8; 32];
    let bulk_operation = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "BulkChallengeMutationOperations"
             (game_id, operation_id, actor_user_id, expected_revision, action,
              challenge_ids, request_digest)
           VALUES (1, $1, $2, 1, 0, ARRAY[10], $3)"#,
    )
    .bind(bulk_operation)
    .bind(actor)
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap();
    let duplicate_bulk = sqlx::query(
        r#"INSERT INTO "BulkChallengeMutationOperations"
             (game_id, operation_id, actor_user_id, expected_revision, action,
              challenge_ids, request_digest)
           VALUES (1, $1, $2, 1, 0, ARRAY[10], $3)"#,
    )
    .bind(bulk_operation)
    .bind(actor)
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        duplicate_bulk
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );

    let division_operation = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "DivisionUpdateOperations"
             (division_id, operation_id, actor_user_id, request_digest,
              expected_revision, result_revision)
           VALUES (20, $1, $2, $3, 1, 1)"#,
    )
    .bind(division_operation)
    .bind(actor)
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap();
    let duplicate_division = sqlx::query(
        r#"INSERT INTO "DivisionUpdateOperations"
             (division_id, operation_id, actor_user_id, request_digest,
              expected_revision, result_revision)
           VALUES (20, $1, $2, $3, 1, 1)"#,
    )
    .bind(division_operation)
    .bind(actor)
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        duplicate_division
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    let invite_operation = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "TeamInviteOperations"
             (team_id, operation_id, actor_user_id, expected_revision,
              result_revision, result_token)
           VALUES (30, $1, $2, 1, 2, $3)"#,
    )
    .bind(invite_operation)
    .bind(actor)
    .bind("a".repeat(32))
    .execute(&pool)
    .await
    .unwrap();
    let duplicate_invite_revision = sqlx::query(
        r#"INSERT INTO "TeamInviteOperations"
             (team_id, operation_id, actor_user_id, expected_revision,
              result_revision, result_token)
           VALUES (30, $1, $2, 1, 2, $3)"#,
    )
    .bind(uuid::Uuid::new_v4())
    .bind(actor)
    .bind("b".repeat(32))
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        duplicate_invite_revision
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    let flag_operation = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "FlagImportOperations"
             (challenge_id, operation_id, actor_user_id, request_digest, lease_token)
           VALUES (10, $1, $2, $3, $4)"#,
    )
    .bind(flag_operation)
    .bind(actor)
    .bind(&digest)
    .bind(uuid::Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    let duplicate_flag_operation = sqlx::query(
        r#"INSERT INTO "FlagImportOperations"
             (challenge_id, operation_id, actor_user_id, request_digest, lease_token)
           VALUES (10, $1, $2, $3, $4)"#,
    )
    .bind(flag_operation)
    .bind(actor)
    .bind(&digest)
    .bind(uuid::Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        duplicate_flag_operation
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );

    let revision: i64 =
        sqlx::query_scalar(r#"SELECT challenge_configuration_revision FROM "Games" WHERE id = 1"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        revision > 1,
        "challenge and flag triggers did not advance revision"
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
