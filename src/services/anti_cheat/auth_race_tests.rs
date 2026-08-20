use std::time::Duration as StdDuration;

use uuid::Uuid;

use super::db_tests::{insert_user, Harness};
use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn canonical_password_admission_rechecks_strengthened_captcha_policy() {
    let harness = Harness::new().await;
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "captcha-user", "192.0.2.1").await;
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key,value)
           VALUES ('AccountPolicy:UseCaptcha','true')"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    let error = admit_existing_user(
        &harness.pool,
        &test_config(),
        user_id,
        Some("captcha-user"),
        Some("192.0.2.1"),
        None,
        IdentitySource::Password,
        "stamp",
        None,
        crate::services::captcha::CaptchaAdmission::Local(None),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    let observations: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "IdentityObservations" WHERE user_id=$1"#)
            .bind(user_id)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(observations, 0);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn captcha_provider_revision_change_while_admission_waits_is_rejected() {
    let harness = Harness::new().await;
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "captcha-revision", "192.0.2.2").await;
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key,value) VALUES
               ('AccountPolicy:UseCaptcha','true'),
               ('CaptchaConfig:Provider','HashPow'),
               ('CaptchaConfig:HashPow:Difficulty','18')"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    let settings = crate::services::captcha::CaptchaSettings::load(&harness.pool, false)
        .await
        .unwrap();
    let proof = crate::services::captcha::CaptchaAdmission::Local(Some(settings.revision()));

    let mut update = harness.pool.begin().await.unwrap();
    lock_policy_update(&mut update).await.unwrap();
    sqlx::query(
        r#"UPDATE "Configs" SET value='19'
            WHERE config_key='CaptchaConfig:HashPow:Difficulty'"#,
    )
    .execute(&mut *update)
    .await
    .unwrap();
    let pool = harness.pool.clone();
    let admission = tokio::spawn(async move {
        admit_existing_user(
            &pool,
            &test_config(),
            user_id,
            Some("captcha-revision"),
            Some("192.0.2.2"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            proof,
        )
        .await
    });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    update.commit().await.unwrap();
    let error = tokio::time::timeout(StdDuration::from_secs(5), admission)
        .await
        .expect("admission did not resume after captcha policy update")
        .unwrap()
        .expect_err("proof from the old captcha revision was accepted");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    let observations: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "IdentityObservations" WHERE user_id=$1"#)
            .bind(user_id)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(observations, 0);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn oauth_provider_admission_is_explicitly_captcha_exempt() {
    let harness = Harness::new().await;
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "oauth-captcha", "192.0.2.3").await;
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key,value) VALUES
               ('AccountPolicy:UseCaptcha','true'),
               ('CaptchaConfig:Provider','HashPow'),
               ('CaptchaConfig:HashPow:Difficulty','18')"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    assert_eq!(
        admit_existing_user(
            &harness.pool,
            &test_config(),
            user_id,
            Some("oauth-captcha"),
            Some("192.0.2.3"),
            None,
            IdentitySource::OAuth,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::OAuthProvider,
        )
        .await
        .unwrap(),
        AdmissionOutcome::Accepted
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn recovery_captcha_recheck_rejects_a_revision_changed_during_verification() {
    let harness = Harness::new().await;
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key,value) VALUES
               ('AccountPolicy:UseCaptcha','true'),
               ('CaptchaConfig:Provider','HashPow'),
               ('CaptchaConfig:HashPow:Difficulty','18')"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    let settings = crate::services::captcha::CaptchaSettings::load(&harness.pool, false)
        .await
        .unwrap();
    let proof = crate::services::captcha::CaptchaAdmission::Local(Some(settings.revision()));

    let mut update = harness.pool.begin().await.unwrap();
    lock_policy_update(&mut update).await.unwrap();
    sqlx::query(
        r#"UPDATE "Configs" SET value='20'
            WHERE config_key='CaptchaConfig:HashPow:Difficulty'"#,
    )
    .execute(&mut *update)
    .await
    .unwrap();
    let pool = harness.pool.clone();
    let recheck =
        tokio::spawn(
            async move { authorize_captcha_admission(&pool, &test_config(), proof).await },
        );
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    update.commit().await.unwrap();
    let error = tokio::time::timeout(StdDuration::from_secs(5), recheck)
        .await
        .expect("recovery recheck did not resume after policy update")
        .unwrap()
        .expect_err("recovery accepted proof from the old captcha revision");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn deconfirmation_while_admission_waits_rolls_back_identity_observation() {
    let harness = Harness::new().await;
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "deconfirmed", "192.0.2.8").await;
    let before: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar(r#"SELECT last_signed_in_utc FROM "AspNetUsers" WHERE id=$1"#)
            .bind(user_id)
            .fetch_one(&harness.pool)
            .await
            .unwrap();

    let mut admin = harness.pool.begin().await.unwrap();
    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email_confirmed=FALSE, security_stamp='rotated'
            WHERE id=$1"#,
    )
    .bind(user_id)
    .execute(&mut *admin)
    .await
    .unwrap();
    let pool = harness.pool.clone();
    let admission = tokio::spawn(async move {
        admit_existing_user(
            &pool,
            &test_config(),
            user_id,
            Some("deconfirmed"),
            Some("198.51.100.9"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
    });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    admin.commit().await.unwrap();
    assert!(tokio::time::timeout(StdDuration::from_secs(5), admission)
        .await
        .expect("admission did not resume after account update")
        .unwrap()
        .is_err());

    let (after, observations): (chrono::DateTime<chrono::Utc>, i64) = sqlx::query_as(
        r#"SELECT account.last_signed_in_utc,
                  (SELECT COUNT(*) FROM "IdentityObservations" WHERE user_id=$1)
             FROM "AspNetUsers" account WHERE account.id=$1"#,
    )
    .bind(user_id)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(after, before);
    assert_eq!(observations, 0);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn oauth_email_reassignment_while_admission_waits_is_rejected() {
    let harness = Harness::new().await;
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "oauth-user", "192.0.2.8").await;
    sqlx::query(r#"UPDATE "AspNetUsers" SET normalized_email='OLD@EXAMPLE.TEST' WHERE id=$1"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();

    let mut admin = harness.pool.begin().await.unwrap();
    // Keep the same stamp here so the expected provider email is independently
    // proven by the canonical row guard. The real admin path also rotates it.
    sqlx::query(r#"UPDATE "AspNetUsers" SET normalized_email='NEW@EXAMPLE.TEST' WHERE id=$1"#)
        .bind(user_id)
        .execute(&mut *admin)
        .await
        .unwrap();
    let pool = harness.pool.clone();
    let admission = tokio::spawn(async move {
        admit_existing_user(
            &pool,
            &test_config(),
            user_id,
            Some("oauth-user"),
            Some("198.51.100.10"),
            None,
            IdentitySource::OAuth,
            "stamp",
            Some("OLD@EXAMPLE.TEST"),
            crate::services::captcha::CaptchaAdmission::OAuthProvider,
        )
        .await
    });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    admin.commit().await.unwrap();
    assert!(tokio::time::timeout(StdDuration::from_secs(5), admission)
        .await
        .expect("OAuth admission did not resume after email reassignment")
        .unwrap()
        .is_err());
    let observations: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "IdentityObservations" WHERE user_id=$1"#)
            .bind(user_id)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(observations, 0);
}

fn test_config() -> AppConfig {
    let mut config = AppConfig::from_env();
    config.identity_hash_key = "identity-race-test-key-0123456789".to_string();
    config
}
