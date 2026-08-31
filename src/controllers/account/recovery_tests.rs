use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn unknown_login_uses_a_valid_dummy_argon2_hash() {
    assert!(argon2::PasswordHash::new(DUMMY_PASSWORD_HASH).is_ok());
    assert!(!crate::utils::crypto_utils::verify_password(
        "any submitted password",
        DUMMY_PASSWORD_HASH,
    ));
}

#[test]
fn email_domain_validation_requires_one_complete_address() {
    assert!(verify_email_domain("user@allowed.test", "allowed.test"));
    assert!(verify_email_domain("user@allowed.test", "ALLOWED.TEST"));
    assert!(!verify_email_domain(
        "user@allowed.test@evil.test",
        "allowed.test"
    ));
    assert!(!verify_email_domain("@allowed.test", "allowed.test"));
    assert!(!verify_email_domain("user@", ""));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn email_change_rechecks_identity_after_a_registration_lock_wait() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_email_identity_{}", Uuid::new_v4().simple());
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
    sqlx::query(
        r#"CREATE TABLE "AspNetUsers" (
             id UUID PRIMARY KEY,
             email TEXT,
             normalized_email TEXT,
             email_confirmed BOOLEAN NOT NULL DEFAULT FALSE,
             role SMALLINT NOT NULL DEFAULT 1,
             password_hash TEXT,
             security_stamp TEXT
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TABLE "Configs" (
             config_key TEXT PRIMARY KEY,
             value TEXT,
             cache_keys TEXT
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut config = crate::models::internal::configs::AppConfig::from_env();
    config.account.use_captcha = false;
    let changer = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id, email, normalized_email, email_confirmed, password_hash,
              security_stamp)
           VALUES ($1, 'old@example.test', 'OLD@EXAMPLE.TEST', TRUE,
                   'old-password-hash', 'stamp-old')"#,
    )
    .bind(changer)
    .execute(&pool)
    .await
    .unwrap();

    // Model a public/OAuth/admin registration that selected the requested
    // email while holding the shared identity lock but has not committed.
    let mut registration = crate::utils::database::begin_sqlx_transaction(&pool)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *registration)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id, email, normalized_email, email_confirmed, security_stamp)
           VALUES ($1, 'claimed@example.test', 'CLAIMED@EXAMPLE.TEST', TRUE, 'stamp-owner')"#,
    )
    .bind(Uuid::new_v4())
    .execute(&mut *registration)
    .await
    .unwrap();

    let contender = tokio::spawn({
        let pool = pool.clone();
        let config = config.clone();
        async move {
            update_email_serialized(
                &pool,
                &config,
                EmailUpdateRequest {
                    user_id: changer,
                    expected_stamp: "stamp-old",
                    email: "claimed@example.test",
                    normalized_email: "CLAIMED@EXAMPLE.TEST",
                    new_stamp: "stamp-new".to_string(),
                    mode: EmailUpdateMode::Immediate,
                },
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    registration.commit().await.unwrap();

    assert_eq!(
        contender.await.unwrap().unwrap(),
        EmailUpdateOutcome::Conflict
    );
    let changer_identity: (Option<String>, Option<String>) = sqlx::query_as(
        r#"SELECT normalized_email, security_stamp
             FROM "AspNetUsers" WHERE id = $1"#,
    )
    .bind(changer)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        changer_identity,
        (Some("OLD@EXAMPLE.TEST".into()), Some("stamp-old".into()))
    );
    let owners: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "AspNetUsers"
            WHERE normalized_email = 'CLAIMED@EXAMPLE.TEST'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owners, 1);

    let mut policy_update = pool.begin().await.unwrap();
    crate::services::anti_cheat::lock_policy_update(&mut policy_update)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key,value)
           VALUES ('AccountPolicy:EmailConfirmationRequired','true')
           ON CONFLICT (config_key) DO UPDATE SET value=EXCLUDED.value"#,
    )
    .execute(&mut *policy_update)
    .await
    .unwrap();
    let contender = tokio::spawn({
        let pool = pool.clone();
        let config = config.clone();
        async move {
            update_email_serialized(
                &pool,
                &config,
                EmailUpdateRequest {
                    user_id: changer,
                    expected_stamp: "stamp-old",
                    email: "fresh@example.test",
                    normalized_email: "FRESH@EXAMPLE.TEST",
                    new_stamp: "stamp-after-policy".to_string(),
                    mode: EmailUpdateMode::Immediate,
                },
            )
            .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "email update skipped the account-policy lock"
    );
    policy_update.commit().await.unwrap();
    let error = contender
        .await
        .unwrap()
        .expect_err("confirmed email committed after policy strengthening");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    let unchanged: (Option<String>, Option<String>) = sqlx::query_as(
        r#"SELECT normalized_email, security_stamp
             FROM "AspNetUsers" WHERE id=$1"#,
    )
    .bind(changer)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        unchanged,
        (Some("OLD@EXAMPLE.TEST".into()), Some("stamp-old".into()))
    );

    // A request authenticated with stamp-old must not overwrite an admin
    // deconfirmation/stamp rotation that wins while the mutation waits.
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key,value)
           VALUES ('AccountPolicy:EmailConfirmationRequired','false')
           ON CONFLICT (config_key) DO UPDATE SET value=EXCLUDED.value"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut admin_update = pool.begin().await.unwrap();
    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email_confirmed=FALSE, security_stamp='stamp-admin-email'
            WHERE id=$1"#,
    )
    .bind(changer)
    .execute(&mut *admin_update)
    .await
    .unwrap();
    let email_change = tokio::spawn({
        let pool = pool.clone();
        let config = config.clone();
        async move {
            update_email_serialized(
                &pool,
                &config,
                EmailUpdateRequest {
                    user_id: changer,
                    expected_stamp: "stamp-old",
                    email: "new@example.test",
                    normalized_email: "NEW@EXAMPLE.TEST",
                    new_stamp: "stamp-email-request".to_string(),
                    mode: EmailUpdateMode::Immediate,
                },
            )
            .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        !email_change.is_finished(),
        "email mutation skipped row guard"
    );
    admin_update.commit().await.unwrap();
    assert_eq!(
        email_change.await.unwrap().unwrap(),
        EmailUpdateOutcome::StampMismatch
    );

    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email_confirmed=TRUE, security_stamp='stamp-old',
                  password_hash='old-password-hash'
            WHERE id=$1"#,
    )
    .bind(changer)
    .execute(&pool)
    .await
    .unwrap();
    let mut admin_update = pool.begin().await.unwrap();
    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email_confirmed=FALSE, security_stamp='stamp-admin-password'
            WHERE id=$1"#,
    )
    .bind(changer)
    .execute(&mut *admin_update)
    .await
    .unwrap();
    let password_change = tokio::spawn({
        let pool = pool.clone();
        async move {
            update_authenticated_password(
                &pool,
                changer,
                "stamp-old",
                "old-password-hash",
                "new-password-hash".to_string(),
                "stamp-password-request",
            )
            .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        !password_change.is_finished(),
        "password mutation skipped row guard"
    );
    admin_update.commit().await.unwrap();
    assert!(!password_change.await.unwrap().unwrap());
    let final_account: (bool, Option<String>, Option<String>) = sqlx::query_as(
        r#"SELECT email_confirmed,password_hash,security_stamp
             FROM "AspNetUsers" WHERE id=$1"#,
    )
    .bind(changer)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        final_account,
        (
            false,
            Some("old-password-hash".into()),
            Some("stamp-admin-password".into())
        )
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
